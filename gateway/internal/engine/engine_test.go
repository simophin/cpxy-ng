package engine

import (
	"encoding/binary"
	"errors"
	"net"
	"net/netip"
	"testing"
	"time"

	"github.com/simophin/cpxy-ng/gateway/internal/sockdev"
	"github.com/tailscale/wireguard-go/tun"
	"golang.org/x/net/dns/dnsmessage"
)

var answer = netip.MustParseAddr("93.184.216.34")

// fakeUpstream answers every A query with answer.
func fakeUpstream(t *testing.T) string {
	t.Helper()
	conn, err := net.ListenPacket("udp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { conn.Close() })
	go func() {
		buf := make([]byte, 4096)
		for {
			n, from, err := conn.ReadFrom(buf)
			if err != nil {
				return
			}
			var q dnsmessage.Message
			if q.Unpack(buf[:n]) != nil || len(q.Questions) != 1 {
				continue
			}
			r := dnsmessage.Message{
				Header:    dnsmessage.Header{ID: q.ID, Response: true, RecursionAvailable: true},
				Questions: q.Questions,
				Answers: []dnsmessage.Resource{{
					Header: dnsmessage.ResourceHeader{Name: q.Questions[0].Name, Type: dnsmessage.TypeA, Class: dnsmessage.ClassINET, TTL: 300},
					Body:   &dnsmessage.AResource{A: answer.As4()},
				}},
			}
			out, _ := r.Pack()
			conn.WriteTo(out, from)
		}
	}()
	return conn.LocalAddr().String()
}

func dnsQuery(t *testing.T, id uint16) []byte {
	t.Helper()
	q := dnsmessage.Message{
		Header: dnsmessage.Header{ID: id, RecursionDesired: true},
		Questions: []dnsmessage.Question{{
			Name: dnsmessage.MustNewName("example.com."), Type: dnsmessage.TypeA, Class: dnsmessage.ClassINET,
		}},
	}
	out, err := q.Pack()
	if err != nil {
		t.Fatal(err)
	}
	return out
}

func checksum(b []byte, sum uint32) uint16 {
	for ; len(b) >= 2; b = b[2:] {
		sum += uint32(binary.BigEndian.Uint16(b))
	}
	if len(b) == 1 {
		sum += uint32(b[0]) << 8
	}
	for sum > 0xffff {
		sum = sum>>16 + sum&0xffff
	}
	return ^uint16(sum)
}

// udpPacket builds an IPv4 UDP packet.
func udpPacket(src, dst netip.AddrPort, payload []byte) []byte {
	p := make([]byte, 28+len(payload))
	p[0], p[8], p[9] = 0x45, 64, 17
	binary.BigEndian.PutUint16(p[2:], uint16(len(p)))
	s, d := src.Addr().As4(), dst.Addr().As4()
	copy(p[12:], s[:])
	copy(p[16:], d[:])
	binary.BigEndian.PutUint16(p[10:], checksum(p[:20], 0))
	udp := p[20:]
	binary.BigEndian.PutUint16(udp[0:], src.Port())
	binary.BigEndian.PutUint16(udp[2:], dst.Port())
	binary.BigEndian.PutUint16(udp[4:], uint16(len(udp)))
	copy(udp[8:], payload)
	pseudo := uint32(17 + len(udp))
	for i := 0; i < 4; i += 2 {
		pseudo += uint32(binary.BigEndian.Uint16(s[i:])) + uint32(binary.BigEndian.Uint16(d[i:]))
	}
	binary.BigEndian.PutUint16(udp[6:], checksum(udp, pseudo))
	return p
}

func answerOf(t *testing.T, response []byte) (uint16, netip.Addr) {
	t.Helper()
	var m dnsmessage.Message
	if err := m.Unpack(response); err != nil {
		t.Fatal(err)
	}
	if len(m.Answers) != 1 {
		t.Fatalf("%d answers", len(m.Answers))
	}
	return m.ID, netip.AddrFrom4(m.Answers[0].Body.(*dnsmessage.AResource).A)
}

// TestAnswersDNSThroughTheDevice runs the device and engine as cpxy-tsgw does: DNS a client
// sends to a public resolver is answered by the engine, and so is DNS over the peer API.
func TestAnswersDNSThroughTheDevice(t *testing.T) {
	upstream := fakeUpstream(t)
	dev, fd, err := sockdev.New(1280)
	if err != nil {
		t.Fatal(err)
	}
	defer dev.Close()
	eng, err := Start(fd, Config{
		Server:         "http://:k@127.0.0.1:1",
		DNSUpstream:    []string{upstream},
		DNSAlternative: []string{upstream},
		MTU:            1280,
	})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Stop()

	client := netip.MustParseAddrPort("100.64.0.7:40000")
	resolver := netip.MustParseAddrPort("8.8.8.8:53")
	packet := udpPacket(client, resolver, dnsQuery(t, 0x1234))
	if _, err := dev.Write([][]byte{packet}, 0); err != nil {
		t.Fatal(err)
	}
	slab := make([]byte, 1280+2*tun.ReadPacketSpacing)
	read := make([]tun.ReadPacket, 1)
	got := make(chan error, 1)
	go func() {
		_, err := dev.Read(slab, read)
		got <- err
	}()
	select {
	case err := <-got:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("no reply")
	}
	reply := slab[read[0].Offset : read[0].Offset+read[0].Size]
	if src := netip.AddrFrom4([4]byte(reply[12:16])); src != resolver.Addr() {
		t.Fatalf("reply from %v", src)
	}
	if id, addr := answerOf(t, reply[28:]); id != 0x1234 || addr != answer {
		t.Fatalf("answer %#x %v", id, addr)
	}

	response, err := eng.Resolve(dnsQuery(t, 0))
	if err != nil {
		t.Fatal(err)
	}
	if _, addr := answerOf(t, response); addr != answer {
		t.Fatalf("resolved %v", addr)
	}
	if sent, received := eng.Traffic(); sent != uint64(len(packet)) || received == 0 {
		t.Fatalf("traffic %d, %d", sent, received)
	}

	if !eng.Running() {
		t.Fatal("not running")
	}
	eng.Stop()
	if eng.Running() {
		t.Fatal("still running")
	}
	if _, err := eng.Resolve(dnsQuery(t, 0)); !errors.Is(err, ErrStopped) {
		t.Fatalf("Resolve after Stop: %v", err)
	}
}

func TestStopsWhenTheDeviceCloses(t *testing.T) {
	dev, fd, err := sockdev.New(1280)
	if err != nil {
		t.Fatal(err)
	}
	upstream := fakeUpstream(t)
	eng, err := Start(fd, Config{Server: "http://:k@127.0.0.1:1", DNSUpstream: []string{upstream}, DNSAlternative: []string{upstream}, MTU: 1280})
	if err != nil {
		t.Fatal(err)
	}
	defer eng.Stop()
	dev.Close()
	deadline := time.Now().Add(5 * time.Second)
	for eng.Running() {
		if time.Now().After(deadline) {
			t.Fatal("engine still running after the device closed")
		}
		time.Sleep(50 * time.Millisecond)
	}
}

func TestReportsStartErrors(t *testing.T) {
	_, fd, err := sockdev.New(1280)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := Start(fd, Config{Server: "not a url", MTU: 1280}); err == nil {
		t.Fatal("started with a bad config")
	}
}
