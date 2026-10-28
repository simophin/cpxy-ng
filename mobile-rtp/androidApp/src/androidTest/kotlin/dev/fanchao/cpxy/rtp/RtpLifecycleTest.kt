package dev.fanchao.cpxy.rtp

import android.content.Intent
import android.net.VpnService
import android.os.ParcelFileDescriptor
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.filters.SdkSuppress
import androidx.test.platform.app.InstrumentationRegistry
import dev.fanchao.cpxy.rtp.engine.RtpException
import dev.fanchao.cpxy.rtp.engine.RtpEngine
import dev.fanchao.cpxy.rtp.engine.RtpState
import dev.fanchao.cpxy.rtp.engine.deriveKey
import dev.fanchao.cpxy.rtp.engine.RtpPlatform
import dev.fanchao.cpxy.rtp.engine.createRtpPreparation
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.net.ServerSocket
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit

/** Run only against tests/local-endpoint.py and a disposable emulator. */
@RunWith(AndroidJUnit4::class)
@SdkSuppress(minSdkVersion = 31)
class RtpLifecycleTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context get() = instrumentation.targetContext
    private val app get() = context.app
    private val profiles = mutableListOf<String>()

    private fun shell(command: String): String {
        // UiAutomation executes argv directly. Feed a real shell so pipes, redirections and
        // printf quoting are interpreted exactly as in an adb shell session.
        val pipes = instrumentation.uiAutomation.executeShellCommandRw("toybox timeout -s KILL 35 sh")
        ParcelFileDescriptor.AutoCloseOutputStream(pipes[1]).use {
            it.write((command + "\nexit\n").encodeToByteArray())
        }
        return ParcelFileDescriptor.AutoCloseInputStream(pipes[0]).bufferedReader().use { it.readText() }
    }

    @Before
    fun foregroundAndGrantPermission() {
        shell("appops set dev.fanchao.cpxy.rtp ACTIVATE_VPN allow")
        if (android.os.Build.VERSION.SDK_INT >= 33) {
            shell("pm grant dev.fanchao.cpxy.rtp android.permission.POST_NOTIFICATIONS")
        }
        instrumentation.startActivitySync(Intent(context, MainActivity::class.java).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        assertNull("VPN permission must be preauthorized on the disposable emulator", VpnService.prepare(context))
    }

    @After
    fun cleanup() = runBlocking {
        app.controller.disconnect()
        awaitState { it == VpnState.Disconnected }
        app.settingsRepository.update { AppSettings() }
        for (id in profiles) {
            app.repository.delete(id)
            app.keys.delete(id)
        }
    }

    private suspend fun profile(endpoint: String = "10.0.2.2:33000"): Profile {
        val profile = Profile(UUID.randomUUID().toString(), "RTP integration", endpoint,
            dnsUpstream = listOf("10.0.2.2:33053"))
        profiles += profile.id
        app.repository.save(profile)
        app.keys.store(profile.id, "android-rtp-test-password")
        return app.repository.connectionProfile(profile.id)
    }

    private suspend fun awaitState(predicate: (VpnState) -> Boolean): VpnState = withTimeout(45_000) {
        app.controller.state.first(predicate)
    }

    private suspend fun connect(profile: Profile) {
        app.controller.connect(profile)
        val state = awaitState {
            it is VpnState.Failed || it == VpnState.Running(profile.id, TunnelState.Connected)
        }
        assertEquals(VpnState.Running(profile.id, TunnelState.Connected), state)
    }

    private fun vpnAddress(): String? = java.net.NetworkInterface.getNetworkInterfaces().toList()
        .filter { it.name.startsWith("tun") && it.isUp }
        .firstNotNullOfOrNull { network ->
            network.inetAddresses.toList().firstOrNull { it is java.net.Inet4Address }?.hostAddress
        }

    @Test
    fun stopInterruptsBlockedNativeHttpWithoutATun() {
        val server = ServerSocket(0)
        val accepted = CountDownLatch(1)
        val release = CountDownLatch(1)
        val executor = Executors.newFixedThreadPool(2)
        val listener = executor.submit {
            server.accept().use { accepted.countDown(); release.await(10, TimeUnit.SECONDS) }
        }
        val json = Profile("native", "native", "127.0.0.1:${server.localPort}").engineProfileJson()
        val operation = createRtpPreparation(json, ByteArray(32), UUID.randomUUID().toString(), object : RtpPlatform {
            override fun protectSocket(fd: Int) = true
        })
        try {
            val preparation = executor.submit<Boolean> {
                try { operation.prepare(); false } catch (_: RtpException.Cancelled) { true }
            }
            assertTrue(accepted.await(5, TimeUnit.SECONDS))
            operation.cancel()
            assertTrue("Native cancellation must interrupt blocked HTTP", preparation.get(4, TimeUnit.SECONDS))
            assertNull(vpnAddress())
        } finally {
            release.countDown()
            operation.discard()
            operation.close()
            server.close()
            listener.get(5, TimeUnit.SECONDS)
            executor.shutdownNow()
        }
    }

    @Test
    fun nativePreparedPathTransfersOnceAndClosesOnlyWorkerDuplicate(): Unit = runBlocking {
        val key = deriveKey("android-rtp-test-password")
        val operation = try {
            createRtpPreparation(Profile("native", "native", "10.0.2.2:33000").engineProfileJson(),
                key, UUID.randomUUID().toString(), object : RtpPlatform {
                    override fun protectSocket(fd: Int) = true
                })
        } finally { key.fill(0) }
        // An idle packet descriptor substitutes for TUN; the service tests cover actual routes.
        val pair = ParcelFileDescriptor.createSocketPair()
        var engine: RtpEngine? = null
        try {
            assertTrue(isIpv4(operation.prepare()))
            val running = operation.start(ParcelFileDescriptor.dup(pair[0].fileDescriptor).detachFd())
            engine = running
            operation.discard()
            withTimeout(5_000) { while (running.status().state != RtpState.CONNECTED) delay(50) }
            val repeatedFd = ParcelFileDescriptor.dup(pair[0].fileDescriptor).detachFd()
            try {
                operation.start(repeatedFd)
                fail("A prepared path must be consumed only once")
            } catch (_: RtpException.Consumed) { }
            try {
                ParcelFileDescriptor.fromFd(repeatedFd).use { fail("Repeated start must close its accepted descriptor") }
            } catch (_: java.io.IOException) { }
            running.stop()
            running.stop()
            assertEquals(RtpState.STOPPED, running.status().state)
            android.system.Os.fstat(pair[0].fileDescriptor) // the retained reference is still valid
        } finally {
            engine?.stop()
            engine?.close()
            operation.discard()
            operation.close()
            pair.forEach { it.close() }
        }
    }

    @Test
    fun stopDuringPreparationAndRapidReplacementCannotResurrectOldSession() = runBlocking {
        val unavailable = profile("10.0.2.2:33999")
        val usable = profile()
        app.controller.connect(unavailable)
        delay(200)
        app.controller.disconnect()
        awaitState { it == VpnState.Disconnected }
        assertNull(vpnAddress())
        repeat(3) {
            app.controller.connect(unavailable)
            app.controller.disconnect()
            connect(usable)
            assertNotNull(vpnAddress())
        }
        delay(2_000)
        assertEquals(VpnState.Running(usable.id, TunnelState.Connected), app.controller.state.value)
    }

    @Test
    fun initialTransientFailureRetriesAndExplicitStopRemovesRoutes() = runBlocking {
        val unavailable = profile("10.0.2.2:33999")
        app.controller.connect(unavailable)
        awaitState { it == VpnState.Running(unavailable.id, TunnelState.Restarting) }
        assertNull(vpnAddress())
        delay(1_500)
        assertFalse(app.controller.state.value is VpnState.Failed)
        app.controller.disconnect()
        awaitState { it == VpnState.Disconnected }
        delay(2_000)
        assertEquals(VpnState.Disconnected, app.controller.state.value)
        assertNull(vpnAddress())
    }

    @Test
    fun assignedAddressIdentityAndProfileReplacement() = runBlocking {
        val first = profile()
        val second = profile()
        assertNotEquals(first.clientId, second.clientId)
        connect(first)
        val firstAddress = vpnAddress()
        assertNotNull(firstAddress)
        val response = shell("printf 'GET / HTTP/1.0\\r\\n\\r\\n' | toybox nc -w 3 203.0.113.2 8080")
        assertTrue("Tunneled TCP peer must match the assigned VPN address: $response", response.contains(firstAddress!!))
        connect(second)
        assertNotEquals(firstAddress, vpnAddress())
        connect(first)
        assertEquals(firstAddress, vpnAddress())
        assertEquals(first.clientId, app.repository.connectionProfile(first.id).clientId)
    }

    private fun tcp(host: String, port: Int) = shell(
        "printf 'GET / HTTP/1.0\\r\\n\\r\\n' | toybox nc -w 3 -W 3 $host $port",
    )

    private fun probe(host: String, port: Int, request: String = ""): String {
        val apk = instrumentation.context.applicationInfo.sourceDir
        return shell("CLASSPATH='$apk' app_process /system/bin dev.fanchao.cpxy.rtp.RtpPacketProbe $host $port $request")
    }

    private fun dns(name: String, type: Int): ByteArray {
        val packet = mutableListOf<Byte>(0x12, 0x34, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0)
        for (label in name.split('.')) {
            packet += label.length.toByte()
            packet += label.encodeToByteArray().toList()
        }
        packet += listOf<Byte>(0, 0, type.toByte(), 0, 1)
        val request = packet.joinToString("") { (it.toInt() and 255).toString(16).padStart(2, '0') }
        val hex = probe("198.18.0.53", 53, request).trim()
        return hex.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
    }

    @Test
    fun tunnelUdpDirectTrafficDnsAndRefusals() = runBlocking {
        connect(profile())
        val address = vpnAddress()!!
        val udp = probe("203.0.113.2", 8080)
        assertTrue("UDP echo should return $address, received: $udp", udp.contains(address))
        assertTrue(tcp("10.0.2.2", 33080).contains("127.0.0.1"))
        assertArrayEquals(byteArrayOf(203.toByte(), 0, 113, 2), dns("remote.test", 1).takeLast(4).toByteArray())
        assertArrayEquals(byteArrayOf(223.toByte(), 5, 5, 5), dns("local.test", 1).takeLast(4).toByteArray())
        val aaaa = dns("remote.test", 28)
        assertTrue(aaaa.size >= 12)
        assertEquals(0, aaaa[6].toInt())
        assertEquals(0, aaaa[7].toInt())
        assertTrue(probe("203.0.113.2", 443).contains("REFUSED"))
        assertTrue(shell("echo hi | toybox nc -6 -w 2 -W 2 2001:db8::1 80 2>&1").isNotBlank())
    }

    @Test
    fun perAppSelectionModesKeepSelfExcluded() = runBlocking {
        val profile = profile()
        for (filter in AppFilter.entries) {
            app.settingsRepository.update { AppSettings(filter, setOf("com.android.shell"), setOf("com.android.shell")) }
            connect(profile)
            val response = tcp("203.0.113.2", 8080)
            if (filter == AppFilter.ExceptSelected) assertFalse(response.contains("200 OK"))
            else assertTrue(response.contains(vpnAddress()!!))
            assertTrue(tcp("10.0.2.2", 33080).contains("200 OK"))
            app.controller.disconnect()
            awaitState { it == VpnState.Disconnected }
        }
    }

    private fun fixture(path: String) {
        val response = shell("printf 'GET /$path HTTP/1.0\\r\\n\\r\\n' | toybox nc -w 3 -W 25 10.0.2.2 33090")
        assertTrue("Local fixture command failed: $response", response.contains("200 OK"))
    }

    @Test
    fun endpointOutageRestartAndChangedAddressRetainCapturedRoutes() = runBlocking {
        val profile = profile()
        connect(profile)
        val previous = vpnAddress()!!
        try {
            fixture("pause")
            // Keep routes and direct traffic while both UDP and HTTP are unresponsive.
            repeat(4) {
                assertEquals(previous, vpnAddress())
                assertFalse(tcp("203.0.113.2", 8080).contains("200 OK"))
                assertTrue(tcp("10.0.2.2", 33080).contains("200 OK"))
                delay(2_000)
            }
        } finally { fixture("resume") }
        withTimeout(45_000) {
            while (!tcp("203.0.113.2", 8080).contains(previous)) delay(500)
        }
        fixture("restart")
        withTimeout(45_000) {
            while (!tcp("203.0.113.2", 8080).contains(previous)) {
                assertEquals(previous, vpnAddress())
                delay(500)
            }
        }
        fixture("reassign")
        withTimeout(60_000) {
            while (vpnAddress() == previous || !tcp("203.0.113.2", 8080).contains(vpnAddress() ?: "invalid")) {
                assertNotNull("A retained or replacement TUN must remain installed", vpnAddress())
                delay(250)
            }
        }
        assertNotEquals(previous, vpnAddress())
        assertEquals(profile.clientId, app.repository.connectionProfile(profile.id).clientId)
    }

    @Test
    fun missingKeyAndWrongIdentityHaveActionablePermanentErrors() = runBlocking {
        val profile = profile()
        app.keys.delete(profile.id)
        app.controller.connect(profile)
        val missing = awaitState { it is VpnState.Failed } as VpnState.Failed
        assertTrue(missing.message.contains("password"))
        delay(500) // Let the previous service destruction finish before starting another one.
        app.keys.store(profile.id, "android-rtp-test-password")
        app.repository.save(profile.copy(serverIdentity = "wrong-server-identity"))
        app.controller.connect(profile)
        val rejected = awaitState { it is VpnState.Failed } as VpnState.Failed
        assertTrue(rejected.message.contains("server identity"))
        assertNull(vpnAddress())
    }

    @Test
    fun permanentReconnectFailureKeepsRoutesUntilExplicitDisconnect() = runBlocking {
        val profile = profile()
        connect(profile)
        val address = vpnAddress()
        app.repository.save(profile.copy(serverIdentity = "wrong-server-identity"))
        app.controller.connect(profile)
        val failure = awaitState { it is VpnState.Failed } as VpnState.Failed
        assertTrue(failure.message.contains("server identity"))
        assertTrue(failure.message.contains("remains blocked"))
        assertEquals(address, vpnAddress())
        assertFalse(tcp("203.0.113.2", 8080).contains("200 OK"))
        assertTrue(tcp("10.0.2.2", 33080).contains("200 OK"))
        app.controller.disconnect()
        awaitState { it == VpnState.Disconnected }
        assertNull(vpnAddress())
    }

    @Test
    fun emulatorNetworkLossAndReturnKeepTunInstalled() = runBlocking {
        val profile = profile()
        connect(profile)
        val address = vpnAddress()!!
        try {
            shell("svc data disable; svc wifi disable")
            repeat(10) {
                assertEquals(address, vpnAddress())
                delay(1_000)
            }
        } finally { shell("svc wifi enable; svc data enable") }
        withTimeout(45_000) {
            while (!tcp("203.0.113.2", 8080).contains(address)) {
                assertEquals(address, vpnAddress())
                delay(500)
            }
        }
    }

    /** Opt in separately and use Android VPN settings to forget this app while the test waits. */
    @Test
    fun permissionRevocationClosesTunAndStopsRetries() = runBlocking {
        org.junit.Assume.assumeTrue(InstrumentationRegistry.getArguments().getString("manualRevoke") == "true")
        connect(profile())
        android.util.Log.i("RtpLifecycleTest", "Ready for VPN settings permission revocation")
        withTimeout(180_000) { app.controller.state.first { it == VpnState.Disconnected } }
        assertNull(vpnAddress())
        delay(2_000)
        assertEquals(VpnState.Disconnected, app.controller.state.value)
    }

    /** Opt in with -e class ...#productionRotation; takes more than five minutes. */
    @Test
    fun productionRotation() = runBlocking {
        org.junit.Assume.assumeTrue(InstrumentationRegistry.getArguments().getString("rotation") == "true")
        val profile = profile()
        connect(profile)
        val address = vpnAddress()!!
        repeat(64) {
            val response = shell("printf 'GET / HTTP/1.0\\r\\n\\r\\n' | toybox nc -w 3 203.0.113.2 8080")
            assertTrue(response.contains(address))
            assertEquals(address, vpnAddress())
            delay(5_000)
        }
    }
}
