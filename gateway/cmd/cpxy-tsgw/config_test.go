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
	if s.hostname != defaultHostname || len(s.engine.DNSAlternative) != 1 {
		t.Fatalf("defaults not applied: %+v", s)
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

func TestRejectsBadSettings(t *testing.T) {
	for name, file := range map[string]map[string]string{
		"no server": {},
		"bad MTU":   {"SERVER": "http://:k@h", "MTU": "big"},
		"bad flag":  {"SERVER": "http://:k@h", "BLOCK_ADS": "maybe"},
	} {
		if _, err := loadSettings(file, noEnv); err == nil {
			t.Errorf("%s: accepted", name)
		}
	}
}
