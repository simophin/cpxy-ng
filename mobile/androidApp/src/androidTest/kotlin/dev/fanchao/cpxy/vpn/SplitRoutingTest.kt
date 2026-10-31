package dev.fanchao.cpxy.vpn

import android.net.VpnService
import android.os.ParcelFileDescriptor
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test

/** Opt-in real VPN smoke test. The probe APK has a separate UID, outside the excluded VPN app UID.
 * Supply vpnServerFile after installing :vpnProbe and granting VPN consent.
 * Credentials are read only into memory and never included in assertions or logs.
 */
class SplitRoutingTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext
    private fun shell(command: String): String =
        ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand(command))
            .bufferedReader().use { it.readText() }

    private fun probe(mode: String, host: String, port: Int, size: Int = 0): String {
        require(host.matches(Regex("[a-zA-Z0-9.:-]+")))
        val output = shell("am broadcast --include-stopped-packages --receiver-foreground -n dev.fanchao.cpxy.probe/.ProbeReceiver --es mode $mode --es host $host --ei port $port --ei size $size")
        return Regex("data=\"([^\"]*)\"").find(output)?.groupValues?.get(1) ?: output.trim()
    }

    private suspend fun connect(profile: Profile) {
        context.app.controller.connect(profile)
        val state = withTimeout(30_000) {
            context.app.controller.state.first { it is VpnState.Connected || it is VpnState.Failed }
        }
        assertTrue("VPN startup failed", state is VpnState.Connected)
        assertEquals("Probe VPN network not ready", "OK", probe("ready", "example.com", 80))
    }

    private suspend fun disconnect() {
        context.app.controller.disconnect()
        withTimeout(10_000) { context.app.controller.state.first { it is VpnState.Disconnected } }
    }

    @Test
    fun externalSocksNativeBypassForcedRoutesAndDirectUdp() = runBlocking {
        val endpoint = InstrumentationRegistry.getArguments().getString("vpnSocksEndpoint")
        assumeTrue("Opt-in external SOCKS fixture not configured", endpoint != null)
        val repository = context.app.settingsRepository
        val previous = repository.settings.first()
        val profile = Profile(
            id = "external-smoke", name = "External smoke", backend = ProfileBackend.Socks5,
            socks5 = endpoint!!, dnsUpstream = listOf("223.5.5.5"), dnsAlternative = listOf("1.1.1.1"),
        )
        try {
            repository.update { AppSettings() }
            connect(profile)
            assertEquals("External SOCKS HTTP", "OK", probe("http", "example.com", 80))
            assertEquals("Native private HTTP", "OK", probe("http", "10.0.2.2", 11081))
            assertEquals("Captured QUIC must be rejected", "OK", probe("blocked", "1.1.1.1", 443))
            disconnect()
            // Force this private IP into TUN. UDP must still be direct even with a dead SOCKS
            // endpoint, including empty datagrams. Private forced TCP must fail through SOCKS.
            repository.update { it.copy(proxyRoutes = listOf("10.0.2.2/32")) }
            connect(profile.copy(socks5 = "127.0.0.1:9"))
            assertEquals("Empty direct UDP with dead SOCKS", "OK", probe("echo", "10.0.2.2", 11082, 0))
            assertEquals("Large direct UDP with dead SOCKS", "OK", probe("echo", "10.0.2.2", 11082, 1400))
            assertEquals("Ordinary overseas UDP with dead SOCKS", "OK", probe("dns", "1.1.1.1", 53))
            assertTrue("Forced TCP leaked direct", probe("http", "10.0.2.2", 11081).startsWith("FAIL"))
            disconnect()
        } finally {
            if (context.app.controller.state.value !is VpnState.Disconnected) disconnect()
            repository.update { previous }
        }
    }

    @Test
    fun cpxyVpnRoutesAppTrafficAndRestarts() = runBlocking {
        val args = InstrumentationRegistry.getArguments()
        val serverFile = args.getString("vpnServerFile")
        assumeTrue("Opt-in VPN fixture not configured", serverFile != null)
        require(serverFile!!.matches(Regex("/data/local/tmp/[a-zA-Z0-9_.-]+")))
        assertNull("Grant VPN consent before running the smoke test", VpnService.prepare(context))
        val server = shell("cat $serverFile").trim()
        check(server.isNotEmpty()) { "Test server file is empty" }
        val repository = context.app.settingsRepository
        val previous = repository.settings.first()
        repository.update { AppSettings() }
        val profile = Profile(
            id = "emulator-smoke", name = "Emulator smoke", server = server,
            dnsUpstream = listOf("223.5.5.5"),
            dnsAlternative = listOf("1.1.1.1"),
            ipv6 = Ipv6Mode.Full,
        )
        try {
            repeat(2) {
                connect(profile)
                assertEquals("Routed HTTP probe failed", "OK", probe("http", "example.com", 80))
                // 10.233.0.2 sits in the private bypass set and must remain captured for DNS.
                assertEquals("Virtual DNS probe failed", "OK", probe("dns", "10.233.0.2", 53))
                assertEquals("Direct UDP probe failed", "OK", probe("dns", "1.1.1.1", 53))
                disconnect()
                assertNull(context.app.controller.localProxy())
            }
        } finally {
            if (context.app.controller.state.value !is VpnState.Disconnected) disconnect()
            repository.update { previous }
        }
    }
}
