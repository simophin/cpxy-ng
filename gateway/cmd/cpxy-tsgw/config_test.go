package main

import (
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func noEnv(string) (string, bool) { return "", false }

func TestReadsTheConfigFile(t *testing.T) {
	path := filepath.Join(t.TempDir(), "cpxy-tsgw.conf")
	os.WriteFile(path, []byte(`# comment
SERVER=https://:key@example.com
DNS_UPSTREAM="223.5.5.5, tcp://119.29.29.29"
DNS_SEARCH_DOMAINS="home.arpa, corp.example"

MTU=1400
BLOCK_ADS=1
`), 0o600)
	file, err := readConfigFile(path)
	if err != nil {
		t.Fatal(err)
	}
	s, err := loadSettings(file, noEnv)
	if err != nil {
		t.Fatal(err)
	}
	if s.engine.Server != "https://:key@example.com" || s.engine.MTU != 1400 || !s.engine.BlockAds {
		t.Fatalf("unexpected settings %+v", s)
	}
	if want := []string{"223.5.5.5", "tcp://119.29.29.29"}; !reflect.DeepEqual(s.engine.DNSUpstream, want) {
		t.Fatalf("DNS_UPSTREAM %q", s.engine.DNSUpstream)
	}
	if want := []string{"home.arpa", "corp.example"}; !reflect.DeepEqual(s.engine.DNSSearchDomains, want) {
		t.Fatalf("DNS_SEARCH_DOMAINS %q", s.engine.DNSSearchDomains)
	}
	if s.hostname != defaultHostname {
		t.Fatalf("defaults not applied: %+v", s)
	}
	if want := []string{"tcp://1.1.1.1?via=cpxy"}; !reflect.DeepEqual(s.engine.DNSAlternative, want) {
		t.Fatalf("DNS_ALTERNATIVE %q", s.engine.DNSAlternative)
	}
}

func TestEnvironmentOverridesTheFile(t *testing.T) {
	env := map[string]string{"SERVER": "http://:a@1.2.3.4", "STATE_DIRECTORY": "/var/lib/cpxy-tsgw"}
	s, err := loadSettings(map[string]string{"SERVER": "http://:b@5.6.7.8"}, func(k string) (string, bool) {
		v, ok := env[k]
		return v, ok
	})
	if err != nil {
		t.Fatal(err)
	}
	if s.engine.Server != "http://:a@1.2.3.4" || s.stateDir != "/var/lib/cpxy-tsgw" || s.engine.MTU != defaultMTU {
		t.Fatalf("unexpected settings %+v", s)
	}
}

func TestPort(t *testing.T) {
	s, err := loadSettings(map[string]string{"SERVER": "http://:k@h", "TS_PORT": "41641"}, noEnv)
	if err != nil {
		t.Fatal(err)
	}
	if s.port != 41641 {
		t.Fatalf("TS_PORT not applied: %+v", s)
	}
	if s, err := loadSettings(map[string]string{"SERVER": "http://:k@h"}, noEnv); err != nil || s.port != 0 {
		t.Fatalf("default port: %v %+v", err, s)
	}
}

func TestRejectsBadSettings(t *testing.T) {
	for name, file := range map[string]map[string]string{
		"no server": {},
		"bad MTU":   {"SERVER": "http://:k@h", "MTU": "big"},
		"bad port":  {"SERVER": "http://:k@h", "TS_PORT": "70000"},
		"bad flag":  {"SERVER": "http://:k@h", "BLOCK_ADS": "maybe"},
	} {
		if _, err := loadSettings(file, noEnv); err == nil {
			t.Errorf("%s: accepted", name)
		}
	}
}

func TestProxyRoutes(t *testing.T) {
	s, err := loadSettings(map[string]string{"SERVER": "http://:k@h", "PROXY_ROUTES": "223.5.5.5/32, 10.0.0.0/8"}, noEnv)
	if err != nil || !reflect.DeepEqual(s.engine.ProxyRoutes, []string{"223.5.5.5/32", "10.0.0.0/8"}) {
		t.Fatalf("proxy routes: %v %+v", err, s.engine.ProxyRoutes)
	}
	for _, route := range []string{"::/0", "10.0.0.0/33", "example.com/24", "1.2.3.4"} {
		if _, err := loadSettings(map[string]string{"SERVER": "http://:k@h", "PROXY_ROUTES": route}, noEnv); err == nil {
			t.Errorf("accepted invalid route %q", route)
		}
	}
}
