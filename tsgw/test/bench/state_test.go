package bench

import (
	"context"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"syscall"
	"testing"
	"time"

	"tailscale.com/tsnet"
	"tailscale.com/tstest/integration/testcontrol"
	"tailscale.com/types/logger"
)

// TestStateFromGo checks that cpxy-tsgw keeps the identity of a node the Go cpxy-tsgw (tsnet)
// registered: started on tsnet's state directory, it is the same node to the control server, not
// a new one. It needs no internet and runs with CPXY_TSGW_BENCH=1, like the benchmark.
func TestStateFromGo(t *testing.T) {
	if os.Getenv("CPXY_TSGW_BENCH") != "1" {
		t.Skip("set CPXY_TSGW_BENCH=1 to run")
	}
	bin := os.Getenv("CPXY_TSGW_BIN")
	if bin == "" {
		bin, _ = filepath.Abs("../../../target/release/cpxy-tsgw")
	}
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Minute)
	defer cancel()

	control := &testcontrol.Server{DERPMap: runDERP(t), Logf: logger.Discard}
	control.HTTPTestServer = httptest.NewUnstartedServer(control)
	control.HTTPTestServer.Start()
	t.Cleanup(control.HTTPTestServer.Close)

	// The node as the Go cpxy-tsgw left it: registered, its state in the directory.
	dir := t.TempDir()
	goNode := &tsnet.Server{Dir: dir, Hostname: "state-gw", ControlURL: control.BaseURL(), Logf: logger.Discard}
	if _, err := goNode.Up(ctx); err != nil {
		t.Fatal(err)
	}
	if err := goNode.Close(); err != nil {
		t.Fatal(err)
	}
	nodes := control.AllNodes()
	if len(nodes) != 1 {
		t.Fatalf("the control server has %d nodes after tsnet, want 1", len(nodes))
	}
	goKey := nodes[0].Key

	logs := &syncBuffer{}
	gw := exec.Command(bin)
	gw.Env = append(os.Environ(),
		"SERVER=http://:unused@127.0.0.1:9",
		"DNS_UPSTREAM=127.0.0.1", "DNS_ALTERNATIVE=127.0.0.1",
		"CONTROL_URL="+control.BaseURL(), "TS_HOSTNAME=state-gw", "STATE_DIR="+dir)
	gw.Stdout, gw.Stderr = logs, logs
	if err := gw.Start(); err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		gw.Process.Signal(syscall.SIGTERM)
		gw.Wait()
		if t.Failed() {
			t.Logf("gateway output:\n%s", logs.String())
		}
	})
	waitFor(t, time.Minute, "the gateway to come up", func() bool {
		return strings.Contains(logs.String(), "Tailscale is Running")
	})
	if !strings.Contains(logs.String(), "Carried the node's Tailscale identity over") {
		t.Error("the gateway did not report carrying the identity over")
	}
	nodes = control.AllNodes()
	if len(nodes) != 1 {
		t.Fatalf("the control server has %d nodes, want the one tsnet registered", len(nodes))
	}
	if nodes[0].Key != goKey {
		t.Errorf("the node key changed: %v, tsnet had %v", nodes[0].Key, goKey)
	}
	if _, err := os.Stat(filepath.Join(dir, "tailscaled.state")); err != nil {
		t.Errorf("tsnet's state is gone: %v", err)
	}
}
