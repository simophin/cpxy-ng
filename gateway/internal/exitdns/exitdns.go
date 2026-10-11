// Package exitdns answers the DNS of clients that use this node as their exit node.
//
// Those clients send DNS as DNS over HTTPS to the exit node's peer API (/dns-query), never as
// packets, so the packet engine does not see it. Tailscale's own handler answers from the first
// nameserver in /etc/resolv.conf; this one answers with the engine's split DNS instead. A peer
// API handler registered for "/" is matched before the built-in /dns-query one, so it takes over
// the whole peer API: the paths other than /dns-query (debug pages and Taildrop) get 404.
package exitdns

import (
	"encoding/base64"
	"io"
	"net/http"
	"strconv"
	"strings"

	"golang.org/x/net/dns/dnsmessage"
	"tailscale.com/ipn/ipnlocal"
	"tailscale.com/util/dnsname"
)

// maxQuery is the largest DNS message.
const maxQuery = 65535

// Resolver answers wire-format DNS queries.
type Resolver interface {
	Resolve(query []byte) ([]byte, error)
}

// Register installs the handler. It must be called once, before the tsnet server starts.
func Register(resolver Resolver) {
	ipnlocal.RegisterPeerAPIHandler("/", handler(resolver, authorize))
}

type authorizer func(ipnlocal.PeerAPIHandler, *http.Request) (bool, func(string) bool)

func handler(resolver Resolver, authorize authorizer) func(ipnlocal.PeerAPIHandler, http.ResponseWriter, *http.Request) {
	return func(h ipnlocal.PeerAPIHandler, w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/dns-query" {
			http.Error(w, "unsupported peerapi path", http.StatusNotFound)
			return
		}
		allowed, allowName := authorize(h, r)
		if !allowed {
			h.Logf("exitdns: DNS from %v denied", h.RemoteAddr())
			http.Error(w, "DNS access denied", http.StatusForbidden)
			return
		}
		serve(w, r, resolver, allowName, h.Logf)
	}
}

// authorize is Tailscale's peerAPIHandler.isPeerAPIDNSAllowed, from what ipnlocal exports: the
// node's own untagged devices may always ask; other peers when a registered hook allows them,
// which by default means this node offers an exit node and its packet filter would let the peer
// reach the internet (0.0.0.0:53). Names the netmap keeps from exit nodes (the tailnet's own
// MagicDNS names) are refused either way.
func authorize(h ipnlocal.PeerAPIHandler, r *http.Request) (bool, func(string) bool) {
	notFiltered := func(name string) bool {
		return allowedByNetmap(h.LocalBackend(), name)
	}
	if h.IsSelfUntagged() {
		return true, notFiltered
	}
	if !h.RemoteAddr().IsValid() {
		return false, nil
	}
	var filters []ipnlocal.DNSNameFilter
	for _, hook := range ipnlocal.HookReplyToDNSQueries {
		allow, filter := hook(h, r)
		if !allow {
			continue
		}
		if filter == nil {
			return true, notFiltered
		}
		filters = append(filters, filter)
	}
	if len(filters) == 0 {
		return false, nil
	}
	return true, func(name string) bool {
		if !notFiltered(name) {
			return false
		}
		for _, f := range filters {
			if f(name) {
				return true
			}
		}
		return false
	}
}

// allowedByNetmap is LocalBackend.allowExitNodeDNSProxyToServeName, which is not exported.
func allowedByNetmap(b *ipnlocal.LocalBackend, name string) bool {
	nm := b.NetMapNoPeers()
	if nm == nil {
		return false
	}
	name = strings.ToLower(name)
	for _, bad := range nm.DNS.ExitNodeFilteredSet {
		switch {
		case bad == "":
		case bad[0] == '.':
			if dnsname.HasSuffix(name, bad) {
				return false
			}
		case name == bad:
			return false
		}
	}
	return true
}

// serve answers one DNS over HTTPS request (RFC 8484), GET or POST.
// logf is the peer API's debug log (cpxy-tsgw --verbose).
func serve(w http.ResponseWriter, r *http.Request, resolver Resolver, allowName func(string) bool, logf func(string, ...any)) {
	query, publicErr := readQuery(r)
	if publicErr != "" {
		http.Error(w, publicErr, http.StatusBadRequest)
		return
	}
	var parser dnsmessage.Parser
	header, err := parser.Start(query)
	if err != nil {
		http.Error(w, "invalid DNS query", http.StatusBadRequest)
		return
	}
	question, err := parser.Question()
	if err != nil {
		http.Error(w, "invalid DNS query", http.StatusBadRequest)
		return
	}

	logf("exitdns: %s %v from %v", question.Name, question.Type, r.RemoteAddr)
	var response []byte
	if allowName(question.Name.String()) {
		response, err = resolver.Resolve(query)
	} else {
		response, err = refused(header, question)
	}
	if err != nil {
		http.Error(w, "DNS forwarding error", http.StatusInternalServerError)
		return
	}
	w.Header().Set("Content-Type", "application/dns-message")
	w.Header().Set("Content-Length", strconv.Itoa(len(response)))
	w.Write(response)
}

func readQuery(r *http.Request) (query []byte, publicErr string) {
	switch r.Method {
	case http.MethodGet:
		q64 := r.FormValue("dns")
		if q64 == "" {
			return nil, "missing 'dns' parameter"
		}
		if base64.RawURLEncoding.DecodedLen(len(q64)) > maxQuery {
			return nil, "query too large"
		}
		q, err := base64.RawURLEncoding.DecodeString(q64)
		if err != nil {
			return nil, "invalid 'dns' base64 encoding"
		}
		return q, ""
	case http.MethodPost:
		if r.Header.Get("Content-Type") != "application/dns-message" {
			return nil, "unexpected Content-Type"
		}
		q, err := io.ReadAll(io.LimitReader(r.Body, maxQuery+1))
		if err != nil {
			return nil, "error reading post body with DNS query"
		}
		if len(q) > maxQuery {
			return nil, "query too large"
		}
		return q, ""
	default:
		return nil, "bad HTTP method"
	}
}

// refused answers a query for a name exit nodes must not resolve, as Tailscale does.
func refused(query dnsmessage.Header, question dnsmessage.Question) ([]byte, error) {
	b := dnsmessage.NewBuilder(nil, dnsmessage.Header{
		ID:               query.ID,
		Response:         true,
		OpCode:           query.OpCode,
		RecursionDesired: query.RecursionDesired,
		RCode:            dnsmessage.RCodeRefused,
	})
	if err := b.StartQuestions(); err != nil {
		return nil, err
	}
	if err := b.Question(question); err != nil {
		return nil, err
	}
	return b.Finish()
}
