package sockdev

import (
	"bytes"
	"os"
	"testing"
	"time"

	"github.com/tailscale/wireguard-go/tun"
)

func newDevice(t *testing.T) (*Device, *os.File) {
	t.Helper()
	d, fd, err := New(1280)
	if err != nil {
		t.Fatal(err)
	}
	peer := os.NewFile(uintptr(fd), "engine")
	t.Cleanup(func() {
		d.Close()
		peer.Close()
	})
	return d, peer
}

func TestReadsOnePacketWithSpacing(t *testing.T) {
	d, peer := newDevice(t)
	packets := [][]byte{bytes.Repeat([]byte{0x45}, 60), bytes.Repeat([]byte{0x46}, 1280)}
	go func() {
		for _, p := range packets {
			peer.Write(p)
		}
	}()

	for _, want := range packets {
		slab := make([]byte, 1280+2*tun.ReadPacketSpacing)
		read := make([]tun.ReadPacket, 1)
		n, err := d.Read(slab, read)
		if err != nil || n != 1 {
			t.Fatalf("Read = %d, %v", n, err)
		}
		if read[0].Offset != tun.ReadPacketSpacing {
			t.Fatalf("offset %d", read[0].Offset)
		}
		if got := slab[read[0].Offset : read[0].Offset+read[0].Size]; !bytes.Equal(got, want) {
			t.Fatalf("read %d bytes, want %d", len(got), len(want))
		}
	}
}

func TestWritesEachBufferAsOnePacket(t *testing.T) {
	d, peer := newDevice(t)
	const offset = 16
	bufs := [][]byte{
		append(make([]byte, offset), []byte("first packet")...),
		append(make([]byte, offset), []byte("second")...),
	}
	if n, err := d.Write(bufs, offset); err != nil || n != 2 {
		t.Fatalf("Write = %d, %v", n, err)
	}
	buf := make([]byte, 100)
	for _, want := range []string{"first packet", "second"} {
		n, err := peer.Read(buf)
		if err != nil {
			t.Fatal(err)
		}
		if got := string(buf[:n]); got != want {
			t.Fatalf("got %q, want %q", got, want)
		}
	}
}

func TestReportsUpAndClosesCleanly(t *testing.T) {
	d, _ := newDevice(t)
	if ev := <-d.Events(); ev != tun.EventUp {
		t.Fatalf("first event %v, want EventUp", ev)
	}
	if mtu, _ := d.MTU(); mtu != 1280 {
		t.Fatalf("MTU %d", mtu)
	}

	done := make(chan error, 1)
	go func() {
		_, err := d.Read(make([]byte, 2000), make([]tun.ReadPacket, 1))
		done <- err
	}()
	time.Sleep(50 * time.Millisecond)
	d.Close()
	select {
	case err := <-done:
		if err == nil {
			t.Fatal("Read after Close succeeded")
		}
	case <-time.After(2 * time.Second):
		t.Fatal("Close did not unblock Read")
	}
	if _, ok := <-d.Events(); ok {
		t.Fatal("events channel still open")
	}
	d.Close()
}

func TestReadFailsWhenTheEngineCloses(t *testing.T) {
	d, peer := newDevice(t)
	peer.Close()
	if _, err := d.Read(make([]byte, 2000), make([]tun.ReadPacket, 1)); err == nil {
		t.Fatal("Read succeeded after the engine end closed")
	}
}
