package exitdns

import (
	"bytes"
	"encoding/base64"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"net/netip"
	"testing"

	"golang.org/x/net/dns/dnsmessage"
	"tailscale.com/ipn/ipnlocal"
)

// echoResolver answers with the query itself, marked as a response.
type echoResolver struct{ calls int }

func (e *echoResolver) Resolve(query []byte) ([]byte, error) {
	e.calls++
	if len(query) < 3 {
		return nil, errors.New("short")
	}
	response := bytes.Clone(query)
	response[2] |= 0x80
	return response, nil
}

func query(t *testing.T, name string) []byte {
	t.Helper()
	b := dnsmessage.NewBuilder(nil, dnsmessage.Header{RecursionDesired: true})
	b.StartQuestions()
	b.Question(dnsmessage.Question{
		Name:  dnsmessage.MustNewName(name),
		Type:  dnsmessage.TypeA,
		Class: dnsmessage.ClassINET,
	})
	q, err := b.Finish()
	if err != nil {
		t.Fatal(err)
	}
	return q
}

func allowAll(string) bool { return true }

func TestAnswersGET(t *testing.T) {
	resolver := &echoResolver{}
	q := query(t, "example.com.")
	r := httptest.NewRequest("GET", "/dns-query?dns="+base64.RawURLEncoding.EncodeToString(q), nil)
	w := httptest.NewRecorder()
	serve(w, r, resolver, allowAll, t.Logf)

	if w.Code != http.StatusOK || w.Header().Get("Content-Type") != "application/dns-message" {
		t.Fatalf("status %d, content type %q", w.Code, w.Header().Get("Content-Type"))
	}
	var msg dnsmessage.Message
	if err := msg.Unpack(w.Body.Bytes()); err != nil || !msg.Response {
		t.Fatalf("bad response %v: %v", msg.Header, err)
	}
	if resolver.calls != 1 {
		t.Fatalf("resolver called %d times", resolver.calls)
	}
}

func TestAnswersPOST(t *testing.T) {
	q := query(t, "example.com.")
	r := httptest.NewRequest("POST", "/dns-query", bytes.NewReader(q))
	r.Header.Set("Content-Type", "application/dns-message")
	w := httptest.NewRecorder()
	serve(w, r, &echoResolver{}, allowAll, t.Logf)
	if w.Code != http.StatusOK || w.Body.Len() != len(q) {
		t.Fatalf("status %d, %d bytes", w.Code, w.Body.Len())
	}
}

func TestRefusesFilteredNames(t *testing.T) {
	resolver := &echoResolver{}
	q := query(t, "host.tailnet.ts.net.")
	r := httptest.NewRequest("POST", "/dns-query", bytes.NewReader(q))
	r.Header.Set("Content-Type", "application/dns-message")
	w := httptest.NewRecorder()
	serve(w, r, resolver, func(name string) bool { return name != "host.tailnet.ts.net." }, t.Logf)

	var msg dnsmessage.Message
	if err := msg.Unpack(w.Body.Bytes()); err != nil {
		t.Fatal(err)
	}
	if msg.RCode != dnsmessage.RCodeRefused || len(msg.Questions) != 1 || resolver.calls != 0 {
		t.Fatalf("got %v with %d questions, resolver called %d times", msg.RCode, len(msg.Questions), resolver.calls)
	}
}

func TestRejectsBadRequests(t *testing.T) {
	for name, r := range map[string]*http.Request{
		"no dns parameter": httptest.NewRequest("GET", "/dns-query", nil),
		"bad base64":       httptest.NewRequest("GET", "/dns-query?dns=!!", nil),
		"not DNS":          httptest.NewRequest("GET", "/dns-query?dns=AAAA", nil),
		"wrong type":       httptest.NewRequest("POST", "/dns-query", bytes.NewReader(query(t, "a."))),
		"bad method":       httptest.NewRequest("PUT", "/dns-query", nil),
	} {
		w := httptest.NewRecorder()
		serve(w, r, &echoResolver{}, allowAll, t.Logf)
		if w.Code != http.StatusBadRequest {
			t.Errorf("%s: status %d", name, w.Code)
		}
	}
}

func TestReportsResolverErrors(t *testing.T) {
	r := httptest.NewRequest("GET", "/dns-query?dns="+base64.RawURLEncoding.EncodeToString(query(t, "a.")), nil)
	w := httptest.NewRecorder()
	serve(w, r, failingResolver{}, allowAll, t.Logf)
	if w.Code != http.StatusInternalServerError {
		t.Fatalf("status %d", w.Code)
	}
}

type failingResolver struct{}

func (failingResolver) Resolve([]byte) ([]byte, error) { return nil, errors.New("down") }

// fakePeer is just enough of a PeerAPIHandler for the routing and authorization checks.
type fakePeer struct{ ipnlocal.PeerAPIHandler }

func (fakePeer) RemoteAddr() netip.AddrPort { return netip.MustParseAddrPort("100.64.0.2:1234") }
func (fakePeer) Logf(string, ...any)        {}

func TestServesOnlyDNSQueryAndChecksAccess(t *testing.T) {
	allow := func(ipnlocal.PeerAPIHandler, *http.Request) (bool, func(string) bool) { return true, allowAll }
	deny := func(ipnlocal.PeerAPIHandler, *http.Request) (bool, func(string) bool) { return false, nil }
	get := "/dns-query?dns=" + base64.RawURLEncoding.EncodeToString(query(t, "a."))

	for _, tc := range []struct {
		path      string
		authorize authorizer
		want      int
	}{
		{get, allow, http.StatusOK},
		{get, deny, http.StatusForbidden},
		{"/v0/goroutines", allow, http.StatusNotFound},
		{"/dns-query-other", allow, http.StatusNotFound},
	} {
		w := httptest.NewRecorder()
		handler(&echoResolver{}, tc.authorize)(fakePeer{}, w, httptest.NewRequest("GET", tc.path, nil))
		if w.Code != tc.want {
			body, _ := io.ReadAll(w.Body)
			t.Errorf("%s: status %d (%s), want %d", tc.path, w.Code, body, tc.want)
		}
	}
}
