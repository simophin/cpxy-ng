// Package bench measures cpxy-tsgw's packet path without the internet or a real tailnet. It runs
// a Tailscale control server and DERP in process, starts the gateway binary on it, and has a
// tsnet client in this process use it as exit node to download from and upload to a TCP server
// on this host's LAN address, which the engine routes direct. It reports MB/s and the gateway's
// CPU and peak RSS per transfer, so the cost of the path itself shows without any cpxy server.
//
// It only runs with CPXY_TSGW_BENCH=1:
//
//	CPXY_TSGW_BIN          the binary, default target/release/cpxy-tsgw (cargo build --release -p cpxy-tsgw)
//	CPXY_TSGW_BENCH_MB     MB per transfer, default 200
//	CPXY_TSGW_BENCH_RUNS   transfers per direction, default 3
//	CPXY_TSGW_BENCH_PARALLEL  connections sharing each transfer, default 1
//	CPXY_TSGW_BENCH_HOST   the address of this host to serve on, default the first private
//	                       IPv4 address of an interface (an exit node refuses loopback)
//	CPXY_TSGW_BENCH_HOLD   seconds to keep running after the transfers, e.g. to attach a profiler
//	CPXY_TSGW_BENCH_BY_LENGTH=1  downloads stop at the size rather than the end of the stream,
//	                       for builds before the engine sent the FIN reliably
//	CPXY_TSGW_BENCH_LOG    where to save the gateway's output
//	CPXY_TSGW_BENCH_GW_<name>=<value>  sets <name> for the gateway only, e.g. TCP_WINDOW
//
//	CPXY_TSGW_BENCH=1 go test -v -count=1 -timeout 30m ./bench
package bench

import (
	"context"
	"crypto/sha256"
	"crypto/tls"
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
	"syscall"
	"testing"
	"time"

	"tailscale.com/derp/derpserver"
	"tailscale.com/ipn"
	"tailscale.com/net/stun/stuntest"
	"tailscale.com/tailcfg"
	"tailscale.com/tsnet"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/key"
	"tailscale.com/types/logger"
	"tailscale.com/types/nettype"
)

const mb = 1_000_000

func TestThroughput(t *testing.T) {
	if os.Getenv("CPXY_TSGW_BENCH") != "1" {
		t.Skip("set CPXY_TSGW_BENCH=1 to run the benchmark")
	}
	bin := os.Getenv("CPXY_TSGW_BIN")
	if bin == "" {
		bin, _ = filepath.Abs("../../../target/release/cpxy-tsgw")
	}
	size := int64(envInt(t, "CPXY_TSGW_BENCH_MB", 200)) * mb
	runs := envInt(t, "CPXY_TSGW_BENCH_RUNS", 3)
	parallel := envInt(t, "CPXY_TSGW_BENCH_PARALLEL", 1)
	host := os.Getenv("CPXY_TSGW_BENCH_HOST")
	if host == "" {
		host = privateAddr(t)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 25*time.Minute)
	defer cancel()

	// The tailnet: control and DERP in this process. Peers on one host connect directly.
	derpMap := runDERP(t)
	controlLogs := &syncBuffer{}
	control := &testcontrol.Server{DERPMap: derpMap, Logf: func(format string, args ...any) {
		fmt.Fprintf(controlLogs, format+"\n", args...)
	}}
	t.Cleanup(func() {
		if t.Failed() {
			t.Logf("control output:\n%s", controlLogs.String())
		}
	})
	control.HTTPTestServer = httptest.NewUnstartedServer(control)
	control.HTTPTestServer.Start()
	t.Cleanup(control.HTTPTestServer.Close)

	addr := serveSink(t, host)
	t.Logf("serving on %s", addr)

	// The gateway, as systemd would run it. Its SERVER is never used: the destination is private.
	gwLogs := &syncBuffer{}
	gw := exec.Command(bin)
	if os.Getenv("CPXY_TSGW_BENCH_VERBOSE") == "1" {
		gw.Args = append(gw.Args, "--verbose")
	}
	gw.Env = append(os.Environ(),
		"SERVER=http://:unused@127.0.0.1:9",
		"DNS_UPSTREAM=127.0.0.1", "DNS_ALTERNATIVE=127.0.0.1",
		"CONTROL_URL="+control.BaseURL(), "TS_HOSTNAME=bench-gw", "STATE_DIR="+t.TempDir(),
		// An exit node refuses private addresses; route this one explicitly.
		"CPXY_TSGW_EXTRA_ROUTES="+host+"/32")
	for _, kv := range os.Environ() {
		if rest, ok := strings.CutPrefix(kv, "CPXY_TSGW_BENCH_GW_"); ok {
			gw.Env = append(gw.Env, rest)
		}
	}
	var out io.Writer = gwLogs
	if path := os.Getenv("CPXY_TSGW_BENCH_LOG"); path != "" {
		f, err := os.Create(path)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { f.Close() })
		out = io.MultiWriter(gwLogs, f)
	}
	gw.Stdout, gw.Stderr = out, out
	if err := gw.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		gw.Process.Signal(syscall.SIGTERM)
		gw.Wait()
		if t.Failed() || os.Getenv("CPXY_TSGW_BENCH_VERBOSE") == "1" {
			t.Logf("gateway output:\n%s", gwLogs.String())
		}
	})
	t.Logf("gateway pid %d", gw.Process.Pid)
	waitFor(t, time.Minute, "the gateway to come up", func() bool {
		return strings.Contains(gwLogs.String(), "Tailscale is Running")
	})
	var gwNode *tailcfg.Node
	for _, n := range control.AllNodes() {
		if n.Hostinfo.Hostname() == "bench-gw" {
			gwNode = n
		}
	}
	if gwNode == nil {
		t.Fatal("the gateway did not register")
	}
	control.SetSubnetRoutes(gwNode.Key, []netip.Prefix{
		netip.MustParsePrefix("0.0.0.0/0"), netip.MustParsePrefix("::/0"), netip.MustParsePrefix(host + "/32"),
	})

	client := &tsnet.Server{
		Dir:        t.TempDir(),
		Hostname:   "bench-client",
		ControlURL: control.BaseURL(),
		Ephemeral:  true,
		Logf:       logger.Discard,
	}
	if os.Getenv("CPXY_TSGW_BENCH_VERBOSE") == "1" {
		client.Logf = t.Logf
	}
	defer client.Close()
	if _, err := client.Up(ctx); err != nil {
		t.Fatal(err)
	}
	lc, err := client.LocalClient()
	if err != nil {
		t.Fatal(err)
	}
	var exitNode tailcfg.StableNodeID
	waitFor(t, time.Minute, "the client to see the gateway as an exit node", func() bool {
		st, err := lc.Status(ctx)
		if err != nil {
			return false
		}
		for _, peer := range st.Peer {
			if peer.HostName == "bench-gw" && peer.ExitNodeOption {
				exitNode = peer.ID
				return true
			}
		}
		return false
	})
	if _, err := lc.EditPrefs(ctx, &ipn.MaskedPrefs{Prefs: ipn.Prefs{ExitNodeID: exitNode}, ExitNodeIDSet: true}); err != nil {
		t.Fatal(err)
	}
	waitFor(t, time.Minute, "the gateway to report it is approved", func() bool {
		return strings.Contains(gwLogs.String(), "the exit node is approved")
	})
	// Warm up, and wait for a direct path rather than DERP.
	waitFor(t, time.Minute, "a direct path to the gateway", func() bool {
		took := transferWithin(ctx, t, client, addr, 'D', mb, 30*time.Second)
		st, err := lc.Status(ctx)
		if err != nil {
			return false
		}
		for _, peer := range st.Peer {
			if peer.HostName == "bench-gw" {
				t.Logf("warm-up: 1 MB in %v, path %q, relay %q", took, peer.CurAddr, peer.Relay)
				return peer.CurAddr != ""
			}
		}
		return false
	})

	mon := &monitor{pid: gw.Process.Pid}
	t.Logf("idle: RSS %.1f MB", float64(rss(mon.pid))/mb)
	for _, dir := range []byte{'D', 'U'} {
		name := map[byte]string{'D': "download", 'U': "upload"}[dir]
		for i := range runs {
			r := mon.measure(func() time.Duration {
				if parallel == 1 {
					return transfer(ctx, t, client, addr, dir, size)
				}
				start := time.Now()
				var wg sync.WaitGroup
				for range parallel {
					wg.Go(func() { transfer(ctx, t, client, addr, dir, size/int64(parallel)) })
				}
				wg.Wait()
				return time.Since(start)
			})
			t.Logf("%s %d: %s", name, i+1, r.line(size))
		}
	}
	if hold := envInt(t, "CPXY_TSGW_BENCH_HOLD", 0); hold > 0 {
		t.Logf("holding for %ds: the client dials %s", hold, addr)
		end := time.Now().Add(time.Duration(hold) * time.Second)
		for time.Now().Before(end) {
			transfer(ctx, t, client, addr, 'D', size/4)
		}
	}
}

// runDERP runs a DERP and STUN server on loopback, as tailscale's integration tests do.
func runDERP(t *testing.T) *tailcfg.DERPMap {
	d := derpserver.New(key.NewNode(), logger.Discard)
	srv := httptest.NewUnstartedServer(derpserver.Handler(d))
	srv.Config.ErrorLog = logger.StdLogger(logger.Discard)
	srv.Config.TLSNextProto = map[string]func(*http.Server, *tls.Conn, http.Handler){}
	srv.StartTLS()
	stunAddr, stunCleanup := stuntest.ServeWithPacketListener(t, nettype.Std{})
	t.Cleanup(func() {
		srv.CloseClientConnections()
		srv.Close()
		d.Close()
		stunCleanup()
	})
	// Trusted by its hash, which tailscale-rs accepts without its test-only insecure mode.
	certHash := sha256.Sum256(srv.Certificate().Raw)
	return &tailcfg.DERPMap{Regions: map[tailcfg.DERPRegionID]*tailcfg.DERPRegion{1: {
		RegionID:   1,
		RegionCode: "bench",
		Nodes: []*tailcfg.DERPNode{{
			Name:       "b1",
			RegionID:   1,
			HostName:   "127.0.0.1",
			IPv4:       "127.0.0.1",
			IPv6:       "none",
			STUNPort:   stunAddr.Port,
			DERPPort:   srv.Listener.Addr().(*net.TCPAddr).Port,
			CertName:   "sha256-raw:" + hex.EncodeToString(certHash[:]),
			STUNTestIP: "127.0.0.1",
		}},
	}}}
}

// serveSink serves the benchmark protocol on host: the client sends 'D' or 'U' and a big-endian
// uint64 size; for 'D' the server sends that many bytes, for 'U' it reads them and answers one
// byte.
func serveSink(t *testing.T, host string) string {
	ln, err := net.Listen("tcp", net.JoinHostPort(host, "0"))
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { ln.Close() })
	go func() {
		for {
			c, err := ln.Accept()
			if err != nil {
				return
			}
			go func() {
				defer c.Close()
				var req [9]byte
				if _, err := io.ReadFull(c, req[:]); err != nil {
					return
				}
				n := int64(binary.BigEndian.Uint64(req[1:]))
				switch req[0] {
				case 'D':
					io.CopyN(c, zeros{}, n)
				case 'U':
					if _, err := io.CopyN(io.Discard, c, n); err == nil {
						c.Write([]byte{1})
					}
				}
			}()
		}
	}()
	return ln.Addr().String()
}

type zeros struct{}

func (zeros) Read(p []byte) (int, error) {
	clear(p)
	return len(p), nil
}

// transfer moves size bytes through the client node and returns how long it took.
func transfer(ctx context.Context, t *testing.T, client *tsnet.Server, addr string, dir byte, size int64) time.Duration {
	return transferWithin(ctx, t, client, addr, dir, size, 5*time.Minute)
}

func transferWithin(ctx context.Context, t *testing.T, client *tsnet.Server, addr string, dir byte, size int64, timeout time.Duration) time.Duration {
	start := time.Now()
	c, err := client.Dial(ctx, "tcp", addr)
	if err != nil {
		t.Fatalf("dial %s through the exit node: %v", addr, err)
	}
	defer c.Close()
	c.SetDeadline(time.Now().Add(timeout))
	var req [9]byte
	req[0] = dir
	binary.BigEndian.PutUint64(req[1:], uint64(size))
	if _, err := c.Write(req[:]); err != nil {
		t.Fatal(err)
	}
	switch dir {
	case 'D':
		// To the end of the stream, so a FIN that never comes fails the transfer, unless
		// comparing with a build that has that bug.
		read := io.Copy
		if os.Getenv("CPXY_TSGW_BENCH_BY_LENGTH") == "1" {
			read = func(w io.Writer, r io.Reader) (int64, error) { return io.CopyN(w, r, size) }
		}
		if n, err := read(io.Discard, c); err != nil || n != size {
			t.Fatalf("downloaded %d of %d bytes: %v", n, size, err)
		}
	case 'U':
		if _, err := io.CopyN(c, zeros{}, size); err != nil {
			t.Fatalf("upload: %v", err)
		}
		var ack [1]byte
		if _, err := io.ReadFull(c, ack[:]); err != nil {
			t.Fatalf("upload not acknowledged: %v", err)
		}
	}
	return time.Since(start)
}

type monitor struct{ pid int }

type result struct {
	elapsed   time.Duration
	cpu       time.Duration
	engineCPU time.Duration
	peakRSS   int64
	// clientCPU is this process's, mostly the client node's: when it nears the cores the
	// client can use, the client rather than the gateway limits the rate.
	clientCPU time.Duration
}

func (r result) line(size int64) string {
	secs := r.elapsed.Seconds()
	return fmt.Sprintf("%6.1f MB/s  gateway CPU %5.1f%% of a core (engine threads %5.1f%%, Tailscale %5.1f%%), %.1f CPU-ms/MB, peak RSS %.1f MB; client CPU %5.1f%%",
		float64(size)/mb/secs, 100*r.cpu.Seconds()/secs, 100*r.engineCPU.Seconds()/secs,
		100*(r.cpu-r.engineCPU).Seconds()/secs, float64(r.cpu.Milliseconds())/(float64(size)/mb),
		float64(r.peakRSS)/mb, 100*r.clientCPU.Seconds()/secs)
}

// measure runs f while sampling the gateway's RSS, and the CPU it used meanwhile, split between
// the engine's threads (named cpxy-engine) and the rest's (Tailscale).
func (m *monitor) measure(f func() time.Duration) result {
	total0, engine0 := threadCPU(m.pid)
	client0 := statCPU("/proc/self/stat")
	var peak atomic.Int64
	stop := make(chan struct{})
	var wg sync.WaitGroup
	wg.Go(func() {
		tick := time.NewTicker(50 * time.Millisecond)
		defer tick.Stop()
		for {
			if r := rss(m.pid); r > peak.Load() {
				peak.Store(r)
			}
			select {
			case <-stop:
				return
			case <-tick.C:
			}
		}
	})
	elapsed := f()
	close(stop)
	wg.Wait()
	total1, engine1 := threadCPU(m.pid)
	return result{
		elapsed:   elapsed,
		cpu:       total1 - total0,
		engineCPU: engine1 - engine0,
		peakRSS:   peak.Load(),
		clientCPU: statCPU("/proc/self/stat") - client0,
	}
}

// threadCPU returns the CPU time of the whole process and of its engine threads.
func threadCPU(pid int) (total, engine time.Duration) {
	total = statCPU(fmt.Sprintf("/proc/%d/stat", pid))
	tasks, _ := os.ReadDir(fmt.Sprintf("/proc/%d/task", pid))
	for _, task := range tasks {
		dir := fmt.Sprintf("/proc/%d/task/%s", pid, task.Name())
		comm, _ := os.ReadFile(dir + "/comm")
		if strings.HasPrefix(string(comm), "cpxy-engine") {
			engine += statCPU(dir + "/stat")
		}
	}
	return total, engine
}

// statCPU reads utime+stime from a stat file. Clock ticks are 100 Hz on Linux.
func statCPU(path string) time.Duration {
	b, err := os.ReadFile(path)
	if err != nil {
		return 0
	}
	s := string(b)
	fields := strings.Fields(s[strings.LastIndexByte(s, ')')+1:])
	utime, _ := strconv.ParseInt(fields[11], 10, 64)
	stime, _ := strconv.ParseInt(fields[12], 10, 64)
	return time.Duration(utime+stime) * 10 * time.Millisecond
}

func rss(pid int) int64 {
	b, _ := os.ReadFile(fmt.Sprintf("/proc/%d/status", pid))
	for line := range strings.Lines(string(b)) {
		if rest, ok := strings.CutPrefix(line, "VmRSS:"); ok {
			kb, _ := strconv.ParseInt(strings.Fields(rest)[0], 10, 64)
			return kb * 1000
		}
	}
	return 0
}

func privateAddr(t *testing.T) string {
	addrs, err := net.InterfaceAddrs()
	if err != nil {
		t.Fatal(err)
	}
	for _, a := range addrs {
		p, err := netip.ParsePrefix(a.String())
		if err == nil && p.Addr().Is4() && p.Addr().IsPrivate() {
			return p.Addr().String()
		}
	}
	t.Fatal("no private IPv4 address on this host; set CPXY_TSGW_BENCH_HOST")
	return ""
}

func envInt(t *testing.T, key string, def int) int {
	v := os.Getenv(key)
	if v == "" {
		return def
	}
	n, err := strconv.Atoi(v)
	if err != nil {
		t.Fatalf("%s: %v", key, err)
	}
	return n
}

func waitFor(t *testing.T, timeout time.Duration, what string, done func() bool) {
	t.Helper()
	deadline := time.Now().Add(timeout)
	for !done() {
		if time.Now().After(deadline) {
			t.Fatalf("timed out waiting for %s", what)
		}
		time.Sleep(time.Second)
	}
}

// syncBuffer collects the gateway's output while the test reads it.
type syncBuffer struct {
	mu  sync.Mutex
	buf strings.Builder
}

func (b *syncBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *syncBuffer) String() string {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.String()
}
