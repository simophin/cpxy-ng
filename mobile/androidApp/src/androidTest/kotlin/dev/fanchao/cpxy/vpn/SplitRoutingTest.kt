package dev.fanchao.cpxy.vpn

import android.net.VpnService
import android.os.ParcelFileDescriptor
import androidx.test.platform.app.InstrumentationRegistry
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test

/** Opt-in real VPN smoke test. The probe runs as shell UID, outside the excluded app UID.
 * Supply vpnServerFile and vpnProbe instrumentation args after granting VPN consent.
 * Credentials are read only into memory and never included in assertions or logs.
 */
class SplitRoutingTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext
    private fun shell(command: String): String =
        ParcelFileDescriptor.AutoCloseInputStream(instrumentation.uiAutomation.executeShellCommand(command))
            .bufferedReader().use { it.readText() }

    @Test
    fun cpxyVpnRoutesShellTrafficAndRestarts() = runBlocking {
        val args = InstrumentationRegistry.getArguments()
        val serverFile = args.getString("vpnServerFile")
        val probe = args.getString("vpnProbe")
        assumeTrue("Opt-in VPN fixture not configured", serverFile != null && probe != null)
        require(serverFile!!.matches(Regex("/data/local/tmp/[a-zA-Z0-9_.-]+")))
        require(probe!!.matches(Regex("/data/local/tmp/[a-zA-Z0-9_.-]+")))
        assertNull("Grant VPN consent before running the smoke test", VpnService.prepare(context))
        val server = shell("cat $serverFile").trim()
        check(server.isNotEmpty()) { "Test server file is empty" }
        val profile = Profile(
            id = "emulator-smoke", name = "Emulator smoke", server = server,
            dnsUpstream = listOf("223.5.5.5"),
            dnsAlternative = listOf("1.1.1.1"),
            ipv6 = Ipv6Mode.Full,
        )
        try {
            repeat(2) {
                context.app.controller.connect(profile)
                val connected = withTimeout(30_000) {
                    context.app.controller.state.first { it is VpnState.Connected || it is VpnState.Failed }
                }
                assertTrue("VPN startup failed", connected is VpnState.Connected)
                assertEquals("Routed HTTP probe failed", "OK", shell("$probe http example.com 80").trim())
                // 10.233.0.2 sits in the private bypass set and must remain captured for DNS.
                assertEquals("Virtual DNS probe failed", "OK", shell("$probe dns 10.233.0.2 53").trim())
                assertEquals("Direct UDP probe failed", "OK", shell("$probe dns 1.1.1.1 53").trim())
                delay(1_200)
                val records = context.app.controller.connectionsBefore(null, 100).records
                assertTrue("No successful SOCKS-routed TCP connection", records.any { it.event.outbound == "proxy" && it.event.error == null })
                assertTrue("TUN traffic counters did not advance", context.app.controller.traffic.value.sent > 0)
                context.app.controller.disconnect()
                withTimeout(10_000) { context.app.controller.state.first { it is VpnState.Disconnected } }
                assertNull(context.app.controller.localProxy())
            }
        } finally {
            context.app.controller.disconnect()
        }
    }
}
