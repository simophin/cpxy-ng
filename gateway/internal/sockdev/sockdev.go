// Package sockdev is the tun.Device tsnet reads and writes, made of one end of a SOCK_SEQPACKET
// socketpair whose other end the Rust packet engine owns. Each message is one IP packet, as on a
// TUN, but nothing touches the kernel's network configuration and no privileges are needed.
//
// Packets move in batches: Read takes every packet queued (up to the batch) with one recvmmsg and
// Write sends a batch with sendmmsg, so WireGuard encrypts, decrypts and sends whole batches, and
// a burst costs a few system calls and wake-ups rather than several per packet.
package sockdev

import (
	"errors"
	"fmt"
	"io"
	"os"
	"sync"
	"syscall"
	"unsafe"

	"github.com/tailscale/wireguard-go/tun"
	"golang.org/x/sys/unix"
)

// Name is what the device reports; no interface of that name exists.
const Name = "cpxy0"

// BatchSize is the most packets one Read or one sendmmsg moves, as for a Linux TUN in
// wireguard-go (conn.IdealBatchSize).
const BatchSize = 128

// socketBuffer is what each end asks for as its send and receive buffer: room for a few hundred
// packets in flight each way. The kernel caps it at net.core.wmem_max and rmem_max.
const socketBuffer = 1 << 20

// Device carries packets between tsnet and the engine.
type Device struct {
	file      *os.File
	raw       syscall.RawConn
	mtu       int
	events    chan tun.Event
	closeOnce sync.Once

	// Read is only called by one goroutine at a time (the device has one queue).
	readMsgs []mmsghdr
	readIovs []unix.Iovec

	writeMu   sync.Mutex
	writeMsgs []mmsghdr
	writeIovs []unix.Iovec
}

// mmsghdr is struct mmsghdr from <sys/socket.h>.
type mmsghdr struct {
	hdr unix.Msghdr
	len uint32
}

// New creates the socketpair. It returns the device and the descriptor for the engine, which
// the caller hands over (the engine closes it).
func New(mtu int) (*Device, int, error) {
	fds, err := unix.Socketpair(unix.AF_UNIX, unix.SOCK_SEQPACKET|unix.SOCK_CLOEXEC, 0)
	if err != nil {
		return nil, -1, fmt.Errorf("socketpair: %w", err)
	}
	for _, fd := range fds {
		// Best effort: smaller buffers only mean smaller bursts.
		unix.SetsockoptInt(fd, unix.SOL_SOCKET, unix.SO_SNDBUF, socketBuffer)
		unix.SetsockoptInt(fd, unix.SOL_SOCKET, unix.SO_RCVBUF, socketBuffer)
	}
	// Non-blocking, so os.NewFile uses the netpoller and Close unblocks a pending Read.
	if err := unix.SetNonblock(fds[0], true); err != nil {
		unix.Close(fds[0])
		unix.Close(fds[1])
		return nil, -1, fmt.Errorf("set non-blocking: %w", err)
	}
	file := os.NewFile(uintptr(fds[0]), "cpxy-engine")
	raw, err := file.SyscallConn()
	if err != nil {
		file.Close()
		unix.Close(fds[1])
		return nil, -1, err
	}
	d := &Device{
		file:      file,
		raw:       raw,
		mtu:       mtu,
		events:    make(chan tun.Event, 1),
		readMsgs:  make([]mmsghdr, BatchSize),
		readIovs:  make([]unix.Iovec, BatchSize),
		writeMsgs: make([]mmsghdr, BatchSize),
		writeIovs: make([]unix.Iovec, BatchSize),
	}
	for i := range BatchSize {
		d.readMsgs[i].hdr.Iov = &d.readIovs[i]
		d.readMsgs[i].hdr.SetIovlen(1)
		d.writeMsgs[i].hdr.Iov = &d.writeIovs[i]
		d.writeMsgs[i].hdr.SetIovlen(1)
	}
	d.events <- tun.EventUp
	return d, fds[1], nil
}

// Read reads the packets queued, at least one, into slab: one per MTU-sized slot, with the
// spacing tun.Reader requires around each. It waits while none is queued.
func (d *Device) Read(slab []byte, packets []tun.ReadPacket) (int, error) {
	const spacing = tun.ReadPacketSpacing
	slot := d.mtu + spacing
	count := min(len(packets), BatchSize, (len(slab)-spacing)/slot)
	if count < 1 {
		return 0, tun.ErrTooManySegments
	}
	for i := range count {
		d.readIovs[i].Base = &slab[spacing+i*slot]
		d.readIovs[i].SetLen(d.mtu)
		d.readMsgs[i].hdr.Flags = 0
	}
	received, err := d.mmsg(d.raw.Read, unix.SYS_RECVMMSG, "recvmmsg", d.readMsgs[:count])
	if err != nil {
		return 0, err
	}
	n := 0
	for i, msg := range d.readMsgs[:received] {
		if msg.len == 0 {
			// The engine closed its end. Hand over what came before; the next Read reports it.
			if n == 0 {
				return 0, io.EOF
			}
			break
		}
		if msg.hdr.Flags&unix.MSG_TRUNC != 0 {
			// Larger than the MTU, which the engine never sends.
			continue
		}
		packets[n] = tun.ReadPacket{Offset: spacing + i*slot, Size: int(msg.len)}
		n++
	}
	return n, nil
}

// Write writes each of bufs[i][offset:] as one packet. It waits while the engine's receive
// queue is full rather than dropping, which slows WireGuard down to what the engine takes.
func (d *Device) Write(bufs [][]byte, offset int) (int, error) {
	d.writeMu.Lock()
	defer d.writeMu.Unlock()
	written := 0
	for written < len(bufs) {
		batch := bufs[written:min(len(bufs), written+BatchSize)]
		for i, buf := range batch {
			packet := buf[offset:]
			if len(packet) == 0 {
				// An empty message would read as the end of the stream.
				return written, errors.New("sockdev: empty packet")
			}
			d.writeIovs[i].Base = &packet[0]
			d.writeIovs[i].SetLen(len(packet))
		}
		sent, err := d.mmsg(d.raw.Write, unix.SYS_SENDMMSG, "sendmmsg", d.writeMsgs[:len(batch)])
		written += sent
		if err != nil {
			return written, err
		}
	}
	return written, nil
}

// mmsg runs recvmmsg or sendmmsg on msgs without blocking, and waits on the netpoller (wait is
// RawConn.Read or Write) while nothing can move. It returns how many messages moved.
func (d *Device) mmsg(wait func(func(uintptr) bool) error, trap uintptr, name string, msgs []mmsghdr) (int, error) {
	var n int
	var opErr error
	err := wait(func(fd uintptr) bool {
		for {
			r, _, errno := unix.Syscall6(trap, fd, uintptr(unsafe.Pointer(&msgs[0])), uintptr(len(msgs)), unix.MSG_DONTWAIT, 0, 0)
			switch errno {
			case 0:
				n = int(r)
				return true
			case unix.EINTR:
				continue
			case unix.EAGAIN:
				return false
			default:
				opErr = os.NewSyscallError(name, errno)
				return true
			}
		}
	})
	if err != nil {
		return 0, err
	}
	return n, opErr
}

func (d *Device) File() *os.File           { return d.file }
func (d *Device) MTU() (int, error)        { return d.mtu, nil }
func (d *Device) Name() (string, error)    { return Name, nil }
func (d *Device) Events() <-chan tun.Event { return d.events }
func (d *Device) BatchSize() int           { return BatchSize }

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
