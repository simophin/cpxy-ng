// cpxy-tsgw is a Tailscale exit node that sends its clients' traffic through a cpxy server,
// entirely in user space: tsnet is the Tailscale node, and the packets clients send to the
// internet go over a socketpair to the Rust packet engine (mobile-engine), which proxies TCP,
// relays UDP and answers DNS. Clients' DNS over the peer API is answered by the same engine. See
// docs/cpxy-tsgw.md.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"log"
	"net/netip"
	"os"
	"os/signal"
	"slices"
	"strings"
	"syscall"
	"time"

	"github.com/simophin/cpxy-ng/gateway/internal/engine"
	"github.com/simophin/cpxy-ng/gateway/internal/exitdns"
	"github.com/simophin/cpxy-ng/gateway/internal/sockdev"
	"tailscale.com/ipn"
	"tailscale.com/ipn/ipnstate"
	"tailscale.com/tsnet"
)

const (
	// Also how soon a new login link shows after the node's key expires.
	statusInterval = 5 * time.Second
	engineCheck    = time.Second
)

var exitRoutes = []netip.Prefix{netip.MustParsePrefix("0.0.0.0/0"), netip.MustParsePrefix("::/0")}

func main() {
	log.SetFlags(0) // journald adds timestamps
	if err := run(); err != nil {
		log.Printf("cpxy-tsgw: %v", err)
		os.Exit(1)
	}
}

func run() error {
	configPath := flag.String("config", "", "a file of KEY=value settings; the environment overrides it")
	hostname := flag.String("hostname", "", "the node's name on the tailnet (TS_HOSTNAME, default "+defaultHostname+")")
	stateDir := flag.String("state-dir", "", "where the node keeps its Tailscale state (STATE_DIR)")
	controlURL := flag.String("control-url", "", "the coordination server, e.g. a Headscale URL (CONTROL_URL)")
	verbose := flag.Bool("verbose", false, "log Tailscale's own debug output")
	flag.Parse()

	var file map[string]string
	if *configPath != "" {
		var err error
		if file, err = readConfigFile(*configPath); err != nil {
			return err
		}
	}
	s, err := loadSettings(file, os.LookupEnv)
	if err != nil {
		return err
	}
	for flagValue, setting := range map[*string]*string{hostname: &s.hostname, stateDir: &s.stateDir, controlURL: &s.controlURL} {
		if *flagValue != "" {
			*setting = *flagValue
		}
	}

	dev, engineFD, err := sockdev.New(s.engine.MTU)
	if err != nil {
		return err
	}
	eng, err := engine.Start(engineFD, s.engine)
	if err != nil {
		dev.Close()
		return err
	}
	defer eng.Stop()
	exitdns.Register(eng)

	srv := &tsnet.Server{
		Tun:        dev,
		Dir:        s.stateDir,
		Hostname:   s.hostname,
		ControlURL: s.controlURL,
		// A fixed port lets a router forward it, so peers connect directly more often.
		Port:     s.port,
		UserLogf: userLog,
	}
	if *verbose {
		srv.Logf = log.Printf
	}
	defer srv.Close()

	ctx, stop := signal.NotifyContext(context.Background(), syscall.SIGINT, syscall.SIGTERM)
	defer stop()
	engineDied := make(chan struct{})
	go watchEngine(ctx, eng, engineDied)

	log.Printf("Starting Tailscale as %q; on the first run a login link appears here", s.hostname)
	if _, err := srv.Up(ctx); err != nil {
		return failure(ctx, engineDied, fmt.Errorf("starting Tailscale: %w", err))
	}
	lc, err := srv.LocalClient()
	if err != nil {
		return err
	}
	if _, err := lc.EditPrefs(ctx, &ipn.MaskedPrefs{
		Prefs:              ipn.Prefs{AdvertiseRoutes: exitRoutes},
		AdvertiseRoutesSet: true,
	}); err != nil {
		return failure(ctx, engineDied, fmt.Errorf("advertising the exit node: %w", err))
	}

	reporter := statusReporter{}
	ticker := time.NewTicker(statusInterval)
	defer ticker.Stop()
	for {
		reporter.report(ctx, lc, eng)
		select {
		case <-ctx.Done():
			log.Printf("Stopping")
			return nil
		case <-engineDied:
			return errors.New("the packet engine stopped")
		case <-ticker.C:
		}
	}
}

// userLog logs what tsnet addresses to the user, in our words where tsnet's suggest that an auth
// key is needed: the link alone logs the node in, and the program waits for that.
func userLog(format string, args ...any) {
	if strings.HasPrefix(format, "To start this tsnet server") {
		log.Printf("Log in by opening %s (waiting; set TS_AUTHKEY to log in without a browser)", args[len(args)-1])
		return
	}
	log.Printf(format, args...)
}

// failure prefers to report a dead engine or a signal over the error they caused.
func failure(ctx context.Context, engineDied <-chan struct{}, err error) error {
	select {
	case <-engineDied:
		return errors.New("the packet engine stopped")
	default:
	}
	if ctx.Err() != nil {
		log.Printf("Stopping")
		return nil
	}
	return err
}

// watchEngine closes died when the engine stops by itself, so the service exits and systemd
// restarts it rather than leaving the exit node black-holing traffic.
func watchEngine(ctx context.Context, eng *engine.Engine, died chan<- struct{}) {
	ticker := time.NewTicker(engineCheck)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			if !eng.Running() {
				close(died)
				return
			}
		}
	}
}

type statusSource interface {
	Status(context.Context) (*ipnstate.Status, error)
	StartLoginInteractive(context.Context) error
}

type trafficSource interface {
	Traffic() (sent, received uint64)
}

// statusReporter logs the node's state when it changes: its addresses and whether the exit node
// is approved yet. When the node's key expires it starts a new login, so the link appears in the
// log and clients come back once someone opens it; nothing needs restarting.
type statusReporter struct {
	last string
	// The login started for the current NeedsLogin state, so it is started once.
	loginStarted bool
}

func (r *statusReporter) report(ctx context.Context, lc statusSource, eng trafficSource) {
	st, err := lc.Status(ctx)
	if err != nil {
		if ctx.Err() == nil {
			log.Printf("Reading Tailscale status: %v", err)
		}
		return
	}
	var line string
	switch {
	case st.BackendState == ipn.NeedsLogin.String():
		if !r.loginStarted {
			if err := lc.StartLoginInteractive(ctx); err != nil {
				log.Printf("Starting the login: %v", err)
				return
			}
			r.loginStarted = true
		}
		if st.AuthURL == "" {
			return
		}
		line = fmt.Sprintf("Tailscale needs a login (the node's key expired?): open %s. Clients are cut off until then", st.AuthURL)
	case st.Self == nil:
		r.loginStarted = false
		line = fmt.Sprintf("Tailscale is %s", st.BackendState)
	default:
		r.loginStarted = false
		approved := st.Self.AllowedIPs != nil && slices.ContainsFunc(st.Self.AllowedIPs.AsSlice(), func(p netip.Prefix) bool {
			return p == exitRoutes[0]
		})
		ips := make([]string, 0, len(st.TailscaleIPs))
		for _, ip := range st.TailscaleIPs {
			ips = append(ips, ip.String())
		}
		line = fmt.Sprintf("Tailscale is %s as %s (%s)", st.BackendState, st.Self.DNSName, strings.Join(ips, ", "))
		if approved {
			line += "; the exit node is approved"
		} else {
			line += "; the exit node is NOT approved yet: approve it in the admin console (Edit route settings, Use as exit node)"
		}
	}
	if line != r.last {
		sent, received := eng.Traffic()
		log.Printf("%s. Traffic so far: %d bytes from clients, %d to them", line, sent, received)
		r.last = line
	}
}
