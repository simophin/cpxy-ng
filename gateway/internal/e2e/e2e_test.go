// Package e2e runs cpxy-tsgw on a real tailnet with a tsnet client using it as exit node. It
// needs network access and secrets, so it only runs with CPXY_TSGW_E2E=1:
//
//	TS_API_KEY         a Tailscale API access token (devices and auth keys)
//	CPXY_TSGW_SERVER   the cpxy server URL with its key, the gateway's SERVER
//	CPXY_TSGW_BIN      the binary, default target/release/cpxy-tsgw (gateway/build.sh)
//	CPXY_TSGW_E2E_LOCAL_URL  a local-region (CN) page showing the caller's address, default
//	                   http://ip.3322.net/
//	CPXY_TSGW_E2E_LOG  where to save the gateway's output
//
// It creates ephemeral, pre-approved auth keys and devices named cpxy-tsgw-test-*, and deletes
// them when done.
package e2e

import (
	"bytes"
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"net/netip"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"sync"
	"syscall"
	"testing"
	"time"

	"golang.org/x/net/dns/dnsmessage"
	"tailscale.com/ipn"
	"tailscale.com/tailcfg"
	"tailscale.com/tsnet"
)

const api = "https://api.tailscale.com/api/v2"

func TestExitNode(t *testing.T) {
	if os.Getenv("CPXY_TSGW_E2E") != "1" {
		t.Skip("set CPXY_TSGW_E2E=1 to run against a real tailnet")
	}
	apiKey, server := mustEnv(t, "TS_API_KEY"), mustEnv(t, "CPXY_TSGW_SERVER")
	bin := os.Getenv("CPXY_TSGW_BIN")
	if bin == "" {
		bin, _ = filepath.Abs("../../../target/release/cpxy-tsgw")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Minute)
	defer cancel()
	ts := &tailscaleAPI{key: apiKey}
	suffix := randomSuffix()
	gwName, clientName := "cpxy-tsgw-test-gw-"+suffix, "cpxy-tsgw-test-client-"+suffix

	// The gateway, as systemd would run it.
	gwKey := ts.createAuthKey(t, "cpxy-tsgw e2e gateway")
	gwLogs := &syncBuffer{}
	gw := exec.Command(bin, "--verbose")
	gw.Env = append(os.Environ(), "SERVER="+server, "TS_AUTHKEY="+gwKey, "HOSTNAME="+gwName, "STATE_DIR="+t.TempDir())
	gw.Stdout, gw.Stderr = gwLogs, gwLogs
	if err := gw.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		gw.Process.Signal(syscall.SIGTERM)
		gw.Wait()
		if path := os.Getenv("CPXY_TSGW_E2E_LOG"); path != "" {
			os.WriteFile(path, gwLogs.Bytes(), 0o600)
		}
	})
	waitFor(t, 2*time.Minute, "the gateway to come up", func() bool {
		return strings.Contains(gwLogs.String(), "Tailscale is Running")
	})
	gwDevice := ts.findDevice(t, gwName)
	t.Cleanup(func() { ts.delete(t, "/device/"+gwDevice) })
	ts.post(t, "/device/"+gwDevice+"/routes", map[string]any{"routes": []string{"0.0.0.0/0", "::/0"}}, nil)

	// The client: another node, using the gateway as its exit node.
	client := &tsnet.Server{
		Dir:       t.TempDir(),
		Hostname:  clientName,
		AuthKey:   ts.createAuthKey(t, "cpxy-tsgw e2e client"),
		Ephemeral: true,
		UserLogf:  t.Logf,
	}
	defer client.Close()
	if _, err := client.Up(ctx); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		// Usually gone already: an ephemeral node is removed when it logs out on Close.
		if id, ok := ts.lookupDevice(t, clientName); ok {
			ts.delete(t, "/device/"+id)
		}
	})
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
			if peer.HostName == gwName && peer.ExitNodeOption {
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

	// Through the exit node, a site outside the local region sees the cpxy server's address,
	// and the name is resolved by the gateway over the peer API.
	hostIP := publicIP(t, http.DefaultClient)
	exitIP := publicIP(t, client.HTTPClient())
	t.Logf("this host's public IP: %v; through the exit node: %v", hostIP, exitIP)
	if exitIP == hostIP {
		t.Errorf("traffic through the exit node left from this host's address %v", hostIP)
	}
	if !strings.Contains(gwLogs.String(), "exitdns: ip-api.com.") {
		t.Errorf("the gateway's peer API did not answer the client's DNS")
	}
	if !strings.Contains(gwLogs.String(), "Answering ip-api.com. IN A") {
		t.Errorf("the engine did not answer ip-api.com")
	}

	// DNS sent as packets to a public resolver is answered by the engine too.
	conn, err := client.Dial(ctx, "udp", "8.8.8.8:53")
	if err != nil {
		t.Fatal(err)
	}
	defer conn.Close()
	conn.SetDeadline(time.Now().Add(15 * time.Second))
	query := dnsQuery(t, "example.org.")
	if _, err := conn.Write(query); err != nil {
		t.Fatal(err)
	}
	buf := make([]byte, 1500)
	n, err := conn.Read(buf)
	if err != nil {
		t.Fatalf("no DNS reply through the exit node: %v", err)
	}
	var reply dnsmessage.Message
	if err := reply.Unpack(buf[:n]); err != nil || len(reply.Answers) == 0 {
		t.Fatalf("bad DNS reply (%v): %+v", err, reply)
	}
	waitFor(t, 5*time.Second, "the engine to log the UDP query", func() bool {
		return strings.Contains(gwLogs.String(), "Answering example.org. IN A")
	})

	// A local-region destination goes direct from the gateway, so it sees this host's address.
	// Its name resolves to a local-region address through the upstream DNS servers.
	localURL := os.Getenv("CPXY_TSGW_E2E_LOCAL_URL")
	if localURL == "" {
		localURL = "http://ip.3322.net/"
	}
	body := strings.TrimSpace(get(t, client.HTTPClient(), localURL))
	t.Logf("%s through the exit node: %s", localURL, body)
	if !strings.Contains(body, hostIP.String()) {
		t.Errorf("%s did not see this host's address %v", localURL, hostIP)
	}
}

func mustEnv(t *testing.T, key string) string {
	v := os.Getenv(key)
	if v == "" {
		t.Fatalf("%s is required", key)
	}
	return v
}

func randomSuffix() string {
	b := make([]byte, 3)
	rand.Read(b)
	return hex.EncodeToString(b)
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

func get(t *testing.T, c *http.Client, url string) string {
	t.Helper()
	c = &http.Client{Transport: c.Transport, Timeout: 30 * time.Second}
	resp, err := c.Get(url)
	if err != nil {
		t.Fatalf("GET %s: %v", url, err)
	}
	defer resp.Body.Close()
	body, err := io.ReadAll(resp.Body)
	if err != nil {
		t.Fatal(err)
	}
	return string(body)
}

func publicIP(t *testing.T, c *http.Client) netip.Addr {
	t.Helper()
	var info struct {
		Query   string `json:"query"`
		Country string `json:"country"`
	}
	if err := json.Unmarshal([]byte(get(t, c, "http://ip-api.com/json")), &info); err != nil {
		t.Fatal(err)
	}
	addr, err := netip.ParseAddr(info.Query)
	if err != nil {
		t.Fatal(err)
	}
	return addr
}

func dnsQuery(t *testing.T, name string) []byte {
	q := dnsmessage.Message{
		Header:    dnsmessage.Header{ID: 0x4242, RecursionDesired: true},
		Questions: []dnsmessage.Question{{Name: dnsmessage.MustNewName(name), Type: dnsmessage.TypeA, Class: dnsmessage.ClassINET}},
	}
	b, err := q.Pack()
	if err != nil {
		t.Fatal(err)
	}
	return b
}

type tailscaleAPI struct{ key string }

func (a *tailscaleAPI) do(t *testing.T, method, path string, body, out any) {
	t.Helper()
	var reader io.Reader
	if body != nil {
		b, _ := json.Marshal(body)
		reader = bytes.NewReader(b)
	}
	req, _ := http.NewRequest(method, api+path, reader)
	req.Header.Set("Authorization", "Bearer "+a.key)
	req.Header.Set("Content-Type", "application/json")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		t.Fatalf("%s %s: %v", method, path, err)
	}
	defer resp.Body.Close()
	data, _ := io.ReadAll(resp.Body)
	if resp.StatusCode/100 != 2 {
		t.Fatalf("%s %s: %s: %s", method, path, resp.Status, data)
	}
	if out != nil {
		if err := json.Unmarshal(data, out); err != nil {
			t.Fatal(err)
		}
	}
}

func (a *tailscaleAPI) post(t *testing.T, path string, body, out any) {
	a.do(t, "POST", path, body, out)
}

func (a *tailscaleAPI) delete(t *testing.T, path string) {
	t.Logf("Deleting %s", path)
	a.do(t, "DELETE", path, nil, nil)
}

// createAuthKey creates an ephemeral, pre-approved, single-use key, deleted when the test ends.
func (a *tailscaleAPI) createAuthKey(t *testing.T, description string) string {
	var key struct{ ID, Key string }
	a.post(t, "/tailnet/-/keys", map[string]any{
		"capabilities": map[string]any{"devices": map[string]any{"create": map[string]any{
			"reusable": false, "ephemeral": true, "preauthorized": true,
		}}},
		"expirySeconds": 3600,
		"description":   description,
	}, &key)
	t.Cleanup(func() { a.delete(t, "/tailnet/-/keys/"+key.ID) })
	return key.Key
}

func (a *tailscaleAPI) findDevice(t *testing.T, hostname string) string {
	id, ok := a.lookupDevice(t, hostname)
	if !ok {
		t.Fatalf("no device %s", hostname)
	}
	return id
}

func (a *tailscaleAPI) lookupDevice(t *testing.T, hostname string) (string, bool) {
	var list struct {
		Devices []struct{ ID, Hostname string } `json:"devices"`
	}
	a.do(t, "GET", "/tailnet/-/devices", nil, &list)
	for _, d := range list.Devices {
		if d.Hostname == hostname {
			return d.ID, true
		}
	}
	return "", false
}

// syncBuffer collects the gateway's output while the test reads it.
type syncBuffer struct {
	mu  sync.Mutex
	buf bytes.Buffer
}

func (b *syncBuffer) Write(p []byte) (int, error) {
	b.mu.Lock()
	defer b.mu.Unlock()
	return b.buf.Write(p)
}

func (b *syncBuffer) String() string { return string(b.Bytes()) }

func (b *syncBuffer) Bytes() []byte {
	b.mu.Lock()
	defer b.mu.Unlock()
	return bytes.Clone(b.buf.Bytes())
}
