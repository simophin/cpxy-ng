package sockdev

import (
	"bytes"
	"fmt"
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

func TestReadsQueuedPacketsInOneBatch(t *testing.T) {
	d, peer := newDevice(t)
	var packets [][]byte
	for i := range 5 {
		packets = append(packets, bytes.Repeat([]byte{byte(0x40 + i)}, 100+i*250))
	}
	for _, p := range packets {
		if _, err := peer.Write(p); err != nil {
			t.Fatal(err)
		}
	}

	// Room for three packets: the first Read takes three, the next the other two.
	slab := make([]byte, tun.ReadPacketSpacing+3*(1280+tun.ReadPacketSpacing))
	read := make([]tun.ReadPacket, BatchSize)
	var got [][]byte
	for len(got) < len(packets) {
		n, err := d.Read(slab, read)
		if err != nil {
			t.Fatal(err)
		}
		if want := min(3, len(packets)-len(got)); n != want {
			t.Fatalf("Read = %d packets, want %d", n, want)
		}
		end := 0
		for _, p := range read[:n] {
			if p.Offset < end+tun.ReadPacketSpacing {
				t.Fatalf("packet at %d overlaps the spacing after the previous one (ends at %d)", p.Offset, end)
			}
			end = p.Offset + p.Size
			got = append(got, bytes.Clone(slab[p.Offset:end]))
		}
		if len(slab)-end < tun.ReadPacketSpacing {
			t.Fatalf("no spacing after the last packet")
		}
	}
	for i := range packets {
		if !bytes.Equal(got[i], packets[i]) {
			t.Fatalf("packet %d differs", i)
		}
	}
}

func TestWriteWaitsForRoomInsteadOfDropping(t *testing.T) {
	d, peer := newDevice(t)
	const count = 5000 // far more than the socket buffers hold
	bufs := make([][]byte, count)
	for i := range bufs {
		bufs[i] = bytes.Repeat([]byte{byte(i)}, 1200)
	}
	done := make(chan error, 1)
	go func() {
		n, err := d.Write(bufs, 0)
		if err == nil && n != count {
			err = fmt.Errorf("wrote %d packets", n)
		}
		done <- err
	}()
	buf := make([]byte, 2000)
	for i := range count {
		n, err := peer.Read(buf)
		if err != nil {
			t.Fatal(err)
		}
		if n != 1200 || buf[0] != byte(i) {
			t.Fatalf("packet %d: %d bytes starting %d", i, n, buf[0])
		}
	}
	if err := <-done; err != nil {
		t.Fatal(err)
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
