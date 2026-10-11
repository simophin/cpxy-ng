// Package sockdev is the tun.Device tsnet reads and writes, made of one end of a SOCK_SEQPACKET
// socketpair whose other end the Rust packet engine owns. Each message is one IP packet, as on a
// TUN, but nothing touches the kernel's network configuration and no privileges are needed.
package sockdev

import (
	"fmt"
	"os"
	"sync"

	"github.com/tailscale/wireguard-go/tun"
	"golang.org/x/sys/unix"
)

// Name is what the device reports; no interface of that name exists.
const Name = "cpxy0"

// Device carries packets between tsnet and the engine.
type Device struct {
	file      *os.File
	mtu       int
	events    chan tun.Event
	closeOnce sync.Once
}

// New creates the socketpair. It returns the device and the descriptor for the engine, which
// the caller hands over (the engine closes it).
func New(mtu int) (*Device, int, error) {
	fds, err := unix.Socketpair(unix.AF_UNIX, unix.SOCK_SEQPACKET|unix.SOCK_CLOEXEC, 0)
	if err != nil {
		return nil, -1, fmt.Errorf("socketpair: %w", err)
	}
	// Non-blocking, so os.NewFile uses the netpoller and Close unblocks a pending Read.
	if err := unix.SetNonblock(fds[0], true); err != nil {
		unix.Close(fds[0])
		unix.Close(fds[1])
		return nil, -1, fmt.Errorf("set non-blocking: %w", err)
	}
	d := &Device{
		file:   os.NewFile(uintptr(fds[0]), "cpxy-engine"),
		mtu:    mtu,
		events: make(chan tun.Event, 1),
	}
	d.events <- tun.EventUp
	return d, fds[1], nil
}

// Read reads one packet into slab, leaving the spacing tun.Reader requires around it.
func (d *Device) Read(slab []byte, packets []tun.ReadPacket) (int, error) {
	n, err := d.file.Read(slab[tun.ReadPacketSpacing : len(slab)-tun.ReadPacketSpacing])
	if err != nil {
		return 0, err
	}
	packets[0] = tun.ReadPacket{Offset: tun.ReadPacketSpacing, Size: n}
	return 1, nil
}

// Write writes each of bufs[i][offset:] as one packet. It waits while the engine's receive
// queue is full rather than dropping, which slows WireGuard down to what the engine takes.
func (d *Device) Write(bufs [][]byte, offset int) (int, error) {
	for i, buf := range bufs {
		if _, err := d.file.Write(buf[offset:]); err != nil {
			return i, err
		}
	}
	return len(bufs), nil
}

func (d *Device) File() *os.File           { return d.file }
func (d *Device) MTU() (int, error)        { return d.mtu, nil }
func (d *Device) Name() (string, error)    { return Name, nil }
func (d *Device) Events() <-chan tun.Event { return d.events }
func (d *Device) BatchSize() int           { return 1 }

// IsFakeTun makes tailscale treat the node as userspace-only (tsd.System.IsNetstack), as it is:
// no kernel interface holds the node's Tailscale addresses. Otherwise the peer API tries to
// listen on those addresses in the kernel, fails, and is never served, and with it the DNS that
// exit node clients send (see package exitdns). Netstack still only takes the packets for this
// node's own listeners and peer API; everything else comes to this device.
func (d *Device) IsFakeTun() bool { return true }

func (d *Device) Close() error {
	var err error
	d.closeOnce.Do(func() {
		err = d.file.Close()
		close(d.events)
	})
	return err
}

var _ tun.Device = (*Device)(nil)
