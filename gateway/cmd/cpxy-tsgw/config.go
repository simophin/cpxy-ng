package main

import (
	"bufio"
	"errors"
	"fmt"
	"net/netip"
	"os"
	"strconv"
	"strings"

	"github.com/simophin/cpxy-ng/gateway/internal/engine"
)

const (
	defaultMTU         = 1280
	defaultHostname    = "cpxy-tsgw"
	defaultDNSUpstream = "223.5.5.5,tcp://119.29.29.29"
	// Asked through the cpxy server, so neither poisoned nor blocked
	defaultDNSAlternative = "tcp://1.1.1.1?via=cpxy,tcp://8.8.8.8?via=cpxy"
)

// settings are read from KEY=value pairs: the environment (systemd's EnvironmentFile), over an
// optional config file. SERVER holds the key, so it is never a flag.
type settings struct {
	engine     engine.Config
	hostname   string
	stateDir   string
	controlURL string
	// The UDP port WireGuard listens on; 0 lets Tailscale pick one.
	port uint16
}

// readConfigFile parses KEY=value lines; blank lines and # comments are skipped, and one level
// of matching quotes around a value is removed, as systemd's EnvironmentFile does.
func readConfigFile(path string) (map[string]string, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	values := map[string]string{}
	scanner := bufio.NewScanner(f)
	for line := 1; scanner.Scan(); line++ {
		text := strings.TrimSpace(scanner.Text())
		if text == "" || strings.HasPrefix(text, "#") {
			continue
		}
		key, value, ok := strings.Cut(text, "=")
		if !ok {
			return nil, fmt.Errorf("%s:%d: expected KEY=value", path, line)
		}
		value = strings.TrimSpace(value)
		if len(value) >= 2 && (value[0] == '"' || value[0] == '\'') && value[len(value)-1] == value[0] {
			value = value[1 : len(value)-1]
		}
		values[strings.TrimSpace(key)] = value
	}
	return values, scanner.Err()
}

// loadSettings reads the settings, each from lookup or else file.
func loadSettings(file map[string]string, lookup func(string) (string, bool)) (settings, error) {
	get := func(key, fallback string) string {
		if v, ok := lookup(key); ok && v != "" {
			return v
		}
		if v, ok := file[key]; ok && v != "" {
			return v
		}
		return fallback
	}

	s := settings{
		engine: engine.Config{
			Server:           get("SERVER", ""),
			ProxyRoutes:      splitList(get("PROXY_ROUTES", "")),
			DNSUpstream:      splitList(get("DNS_UPSTREAM", defaultDNSUpstream)),
			DNSAlternative:   splitList(get("DNS_ALTERNATIVE", defaultDNSAlternative)),
			DNSSearchDomains: splitList(get("DNS_SEARCH_DOMAINS", "")),
		},
		// Not HOSTNAME, which shells and containers set to the machine's name.
		hostname: get("TS_HOSTNAME", defaultHostname),
		// systemd's StateDirectory= sets STATE_DIRECTORY.
		stateDir:   get("STATE_DIR", get("STATE_DIRECTORY", "")),
		controlURL: get("CONTROL_URL", ""),
	}
	if s.engine.Server == "" {
		return s, errors.New("SERVER is required: the cpxy server URL with its key")
	}
	for _, route := range s.engine.ProxyRoutes {
		prefix, err := netip.ParsePrefix(route)
		if err != nil || !prefix.Addr().Is4() {
			return s, fmt.Errorf("invalid IPv4 proxy route CIDR: %s", route)
		}
	}
	mtu, err := strconv.Atoi(get("MTU", strconv.Itoa(defaultMTU)))
	if err != nil {
		return s, fmt.Errorf("invalid MTU: %w", err)
	}
	s.engine.MTU = mtu
	port, err := strconv.ParseUint(get("TS_PORT", "0"), 10, 16)
	if err != nil {
		return s, fmt.Errorf("invalid TS_PORT: %w", err)
	}
	s.port = uint16(port)
	switch v := get("BLOCK_ADS", "0"); v {
	case "0", "false", "no":
	case "1", "true", "yes":
		s.engine.BlockAds = true
	default:
		return s, fmt.Errorf("invalid BLOCK_ADS %q: use 0 or 1", v)
	}
	return s, nil
}

// splitList splits on commas and spaces, as cpxy-router's settings do.
func splitList(s string) []string {
	return strings.FieldsFunc(s, func(r rune) bool { return r == ',' || r == ' ' || r == '\t' })
}
