package main

import (
	"context"
	"testing"

	"tailscale.com/ipn"
	"tailscale.com/ipn/ipnstate"
)

type fakeStatus struct {
	status ipnstate.Status
	logins int
}

func (f *fakeStatus) Status(context.Context) (*ipnstate.Status, error) { return &f.status, nil }
func (f *fakeStatus) StartLoginInteractive(context.Context) error      { f.logins++; return nil }
func (f *fakeStatus) Traffic() (uint64, uint64)                        { return 0, 0 }

// An expired key starts one login, and a later login allows another.
func TestReporterStartsLoginWhenNeeded(t *testing.T) {
	src := &fakeStatus{status: ipnstate.Status{BackendState: ipn.NeedsLogin.String()}}
	r := &statusReporter{}
	for range 3 {
		r.report(context.Background(), src, src)
	}
	if src.logins != 1 {
		t.Fatalf("started %d logins, want 1", src.logins)
	}
	src.status = ipnstate.Status{BackendState: ipn.Running.String()}
	r.report(context.Background(), src, src)
	src.status = ipnstate.Status{BackendState: ipn.NeedsLogin.String()}
	r.report(context.Background(), src, src)
	if src.logins != 2 {
		t.Fatalf("started %d logins after a second expiry, want 2", src.logins)
	}
}
