package dev.fanchao.cpxy.rtp;

import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.InetAddress;
import java.net.PortUnreachableException;
import java.net.SocketTimeoutException;
import java.nio.charset.StandardCharsets;

/** app_process entry point: shell UID is captured by the VPN, unlike the app's own UID. */
public final class RtpPacketProbe {
    public static void main(String[] args) throws Exception {
        byte[] request = "probe".getBytes(StandardCharsets.US_ASCII);
        if (args.length == 3) {
            request = new byte[args[2].length() / 2];
            for (int i = 0; i < request.length; i++) {
                request[i] = (byte) Integer.parseInt(args[2].substring(i * 2, i * 2 + 2), 16);
            }
        }
        try (DatagramSocket socket = new DatagramSocket()) {
            socket.connect(InetAddress.getByName(args[0]), Integer.parseInt(args[1]));
            socket.setSoTimeout(3000);
            socket.send(new DatagramPacket(request, request.length));
            DatagramPacket response = new DatagramPacket(new byte[2048], 2048);
            socket.receive(response);
            if (args.length == 3) {
                StringBuilder hex = new StringBuilder();
                for (int i = 0; i < response.getLength(); i++) {
                    hex.append(String.format("%02x", response.getData()[i] & 255));
                }
                System.out.println(hex);
            } else {
                System.out.println(new String(response.getData(), 0, response.getLength(), StandardCharsets.US_ASCII));
            }
        } catch (PortUnreachableException refused) {
            System.out.println("REFUSED");
        } catch (SocketTimeoutException timeout) {
            System.out.println("TIMEOUT");
        }
    }
}
