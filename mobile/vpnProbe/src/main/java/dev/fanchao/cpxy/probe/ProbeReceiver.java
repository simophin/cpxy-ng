package dev.fanchao.cpxy.probe;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.net.ConnectivityManager;
import android.net.NetworkCapabilities;
import java.net.*;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.util.Arrays;

/** Debug-only fixture with its own application UID. Ordered broadcasts return probe results. */
public final class ProbeReceiver extends BroadcastReceiver {
    @Override public void onReceive(Context context, Intent intent) {
        PendingResult result = goAsync();
        new Thread(() -> {
            try {
                ConnectivityManager manager = context.getSystemService(ConnectivityManager.class);
                boolean ready = false;
                for (int i = 0; i < 40; i++) {
                    NetworkCapabilities capabilities = manager.getNetworkCapabilities(manager.getActiveNetwork());
                    if (capabilities != null && capabilities.hasTransport(NetworkCapabilities.TRANSPORT_VPN)) { ready = true; break; }
                    Thread.sleep(100);
                }
                if (!ready) throw new IOException("Probe UID has no active VPN");
                String mode = intent.getStringExtra("mode");
                String host = intent.getStringExtra("host");
                int port = intent.getIntExtra("port", 80);
                if ("ready".equals(mode)) { /* Route installation is visible to this UID. */ }
                else if ("http".equals(mode)) http(host, port);
                else if ("dns".equals(mode)) dns(host, port);
                else if ("echo".equals(mode)) echo(host, port, intent.getIntExtra("size", 0));
                else if ("blocked".equals(mode)) {
                    try { echo(host, port, 8); throw new IOException("UDP was not rejected"); }
                    catch (PortUnreachableException expected) { }
                } else throw new IOException("Unknown probe mode");
                result.setResultData("OK");
            } catch (Exception e) {
                result.setResultData("FAIL " + e.getClass().getSimpleName() + ": " + e.getMessage());
            } finally { result.finish(); }
        }, "vpn-probe").start();
    }

    private static void http(String host, int port) throws Exception {
        try (Socket socket = new Socket()) {
            socket.connect(new InetSocketAddress(host, port), 5000);
            socket.setSoTimeout(5000);
            socket.getOutputStream().write(("GET / HTTP/1.1\r\nHost: " + host + "\r\nConnection: close\r\n\r\n").getBytes(StandardCharsets.US_ASCII));
            String line = new BufferedReader(new InputStreamReader(socket.getInputStream(), StandardCharsets.US_ASCII)).readLine();
            if (line == null || !line.startsWith("HTTP/1.")) throw new IOException("No HTTP response");
        }
    }

    private static void dns(String host, int port) throws Exception {
        byte[] query = {0x43,0x50,1,0,0,1,0,0,0,0,0,0,7,'e','x','a','m','p','l','e',3,'c','o','m',0,0,1,0,1};
        try (DatagramSocket socket = new DatagramSocket()) {
            socket.connect(InetAddress.getByName(host), port);
            socket.setSoTimeout(6000);
            socket.send(new DatagramPacket(query, query.length));
            byte[] bytes = new byte[4096];
            DatagramPacket response = new DatagramPacket(bytes, bytes.length);
            socket.receive(response);
            if (response.getLength() < 12 || bytes[0] != 0x43 || bytes[1] != 0x50 || (bytes[2] & 0x80) == 0 || (bytes[3] & 15) != 0 || (bytes[6] == 0 && bytes[7] == 0)) throw new IOException("Invalid DNS response");
        }
    }

    private static void echo(String host, int port, int size) throws Exception {
        byte[] payload = new byte[size];
        Arrays.fill(payload, (byte) 7);
        try (DatagramSocket socket = new DatagramSocket()) {
            InetAddress address = InetAddress.getByName(host);
            // Android IoBridge suppresses a zero-byte send on a connected socket. An explicit
            // destination on an unconnected socket exercises a real empty UDP datagram.
            if (size != 0) socket.connect(address, port);
            socket.setSoTimeout(5000);
            socket.send(new DatagramPacket(payload, payload.length, address, port));
            byte[] bytes = new byte[65535];
            DatagramPacket response = new DatagramPacket(bytes, bytes.length);
            socket.receive(response);
            if (!Arrays.equals(payload, Arrays.copyOf(bytes, response.getLength()))) throw new IOException("Datagram changed");
        }
    }
}
