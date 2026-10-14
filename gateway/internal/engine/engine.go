// Package engine runs the Rust packet engine (mobile-engine's C ABI, include/cpxy_engine.h) in
// gateway mode. build.sh builds the static library and passes its path, with the system
// libraries it needs, in CGO_LDFLAGS.
package engine

/*
#cgo CFLAGS: -I${SRCDIR}/../../../mobile-engine/include
#cgo LDFLAGS: -lm -lpthread -ldl
#include <stdlib.h>
#include "cpxy_engine.h"
*/
import "C"

import (
	"encoding/json"
	"errors"
	"fmt"
	"sync"
	"unsafe"
)

// Config is the engine's JSON config (mobile-engine/src/config.rs).
type Config struct {
	ProxyRoutes []string `json:"proxy_routes,omitempty"`
	// Server is the cpxy server URL with its key.
	Server           string   `json:"server"`
	DNSUpstream      []string `json:"dns_upstream"`
	DNSAlternative   []string `json:"dns_alternative"`
	DNSSearchDomains []string `json:"dns_search_domains,omitempty"`
	MTU              int      `json:"mtu"`
	BlockAds         bool     `json:"block_ads"`
}

// maxDNSMessage fits any DNS message.
const maxDNSMessage = 65535

var (
	// ErrStopped is returned by Resolve once the engine is stopped.
	ErrStopped = errors.New("engine stopped")
	errFailed  = errors.New("engine could not answer the query")
)

// Engine is a running engine. Its methods may be called from any goroutine.
type Engine struct {
	// Held for reading while calling into the engine and for writing while stopping it: the C
	// ABI forbids stopping during other calls.
	mu     sync.RWMutex
	handle *C.CpxyEngine
}

// Start starts the engine on fd, which it takes over (and closes, even on failure). It blocks
// while the DNS servers are set up.
func Start(fd int, config Config) (*Engine, error) {
	configJSON, err := json.Marshal(config)
	if err != nil {
		return nil, err
	}
	cConfig := C.CString(string(configJSON))
	defer C.free(unsafe.Pointer(cConfig))
	var errBuf [1024]C.char
	handle := C.cpxy_engine_start(C.int(fd), cConfig, &errBuf[0], C.uint32_t(len(errBuf)))
	if handle == nil {
		return nil, fmt.Errorf("starting the packet engine: %s", C.GoString(&errBuf[0]))
	}
	return &Engine{handle: handle}, nil
}

// Stop stops the engine, closing its descriptor, and frees it. Later calls do nothing.
func (e *Engine) Stop() {
	e.mu.Lock()
	defer e.mu.Unlock()
	if e.handle != nil {
		C.cpxy_engine_free(e.handle)
		e.handle = nil
	}
}

// Running reports whether the packet worker still runs.
func (e *Engine) Running() bool {
	e.mu.RLock()
	defer e.mu.RUnlock()
	return e.handle != nil && bool(C.cpxy_engine_is_running(e.handle))
}

// Traffic returns the IP bytes from clients into the engine and back.
func (e *Engine) Traffic() (sent, received uint64) {
	e.mu.RLock()
	defer e.mu.RUnlock()
	var s, r C.uint64_t
	C.cpxy_engine_traffic(e.handle, &s, &r)
	return uint64(s), uint64(r)
}

// Resolve answers a wire-format DNS query with the engine's split DNS. It blocks until
// answered; the engine's DNS servers time out after a few seconds.
func (e *Engine) Resolve(query []byte) ([]byte, error) {
	if len(query) == 0 {
		return nil, errFailed
	}
	e.mu.RLock()
	defer e.mu.RUnlock()
	if e.handle == nil {
		return nil, ErrStopped
	}
	out := make([]byte, maxDNSMessage)
	n := C.cpxy_engine_dns_resolve(e.handle,
		(*C.uint8_t)(unsafe.Pointer(&query[0])), C.size_t(len(query)),
		(*C.uint8_t)(unsafe.Pointer(&out[0])), C.size_t(len(out)))
	switch {
	case n >= 0:
		return out[:n], nil
	case n == C.CPXY_ENGINE_ERR_STOPPED:
		return nil, ErrStopped
	default:
		return nil, errFailed
	}
}
