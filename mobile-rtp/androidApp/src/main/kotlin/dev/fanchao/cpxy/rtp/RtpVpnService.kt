package dev.fanchao.cpxy.rtp

import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Build
import android.os.Handler
import android.os.Looper
import android.net.VpnService
import android.os.SystemClock
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.core.app.NotificationChannelCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import dev.fanchao.cpxy.rtp.engine.RtpEngine
import dev.fanchao.cpxy.rtp.engine.RtpException
import dev.fanchao.cpxy.rtp.engine.RtpPlatform
import dev.fanchao.cpxy.rtp.engine.RtpState
import dev.fanchao.cpxy.rtp.engine.dnsAddress
import dev.fanchao.cpxy.rtp.engine.createRtpPreparation
import dev.fanchao.cpxy.rtp.engine.RtpPreparation
import dev.fanchao.cpxy.rtp.engine.tunnelMtu
import dev.fanchao.cpxy.rtp.engine.tunnelRoutes
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.util.concurrent.atomic.AtomicLong

/**
 * Sets up the TUN device with the engine's routes and runs the engine on it, starting it again
 * with backoff when it fails. The TUN stays up meanwhile, so traffic for the tunnel waits rather
 * than going direct. The app itself is excluded from the VPN, so the engine's sockets bypass it.
 */
class RtpVpnService : VpnService() {
    private val controller get() = app.controller
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    // All ownership transfers and engine calls run on this lane. Cancellation signals are
    // deliberately independent: a blocking UniFFI call must not prevent Disconnect.
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    private val engineDispatcher = Dispatchers.IO.limitedParallelism(1)
    private val transitions = Mutex()
    private val generation = ConnectionGeneration()
    private var runJob: Job? = null
    private var engine: RtpEngine? = null
    @Volatile private var preparation: RtpPreparation? = null
    private var tun: ParcelFileDescriptor? = null
    private var tunConfig: TunConfig? = null

    private data class TunConfig(val address: String, val profileId: String, val settings: AppSettings)

    @Volatile private var network: Network? = null
    private val networkVersion = AtomicLong()
    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) = underlyingChanged(network)
        override fun onLost(network: Network) {
            if (this@RtpVpnService.network == network) underlyingChanged(null)
        }
    }

    private fun underlyingChanged(next: Network?) {
        if (network == next) return
        network = next
        networkVersion.incrementAndGet()
        setUnderlyingNetworks(next?.let { arrayOf(it) })
        // A path prepared on a vanished network must not be handed to the packet worker.
        preparation?.cancel()
        scope.launch(engineDispatcher) { engine?.networkChanged() }
    }

    private val protector = object : RtpPlatform {
        override fun protectSocket(fd: Int): Boolean = protect(fd)
    }

    override fun onCreate() {
        super.onCreate()
        // Not the default network callback: once the VPN is up, the system reports the VPN as this
        // app's default network too, although the app is outside it, and stops reporting changes.
        val request = NetworkRequest.Builder()
            .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
            .build()
        val connectivity = getSystemService(ConnectivityManager::class.java)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            connectivity.registerBestMatchingNetworkCallback(request, networkCallback, Handler(Looper.getMainLooper()))
        } else {
            // Satisfied by the best matching network, which is the default one anyway.
            connectivity.requestNetwork(request, networkCallback)
        }
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val ticket = generation.replace()
        preparation?.cancel()
        runJob?.cancel()
        if (intent?.action == ACTION_STOP) {
            scope.launch(engineDispatcher) {
                transitions.withLock {
                    if (generation.isCurrent(ticket)) stop(VpnState.Disconnected)
                }
            }
            return START_NOT_STICKY
        }

        showNotification("Connecting…")
        val requestedId = intent?.getStringExtra(EXTRA_PROFILE_ID)
        scope.launch(engineDispatcher) {
            transitions.withLock {
                runJob?.cancelAndJoin()
                stopEngine()
                if (!generation.isCurrent(ticket)) return@withLock
                try {
                    val stored = app.repository.profiles.first()
                    val selected = stored.profiles.firstOrNull { it.id == requestedId } ?: stored.selected
                    if (selected == null) {
                        stop(VpnState.Failed("Add a profile to connect"))
                        return@withLock
                    }
                    // The transaction finishes its durable UUID write before any allocation.
                    val profile = app.repository.connectionProfile(selected.id)
                    val settings = app.settingsRepository.settings.first()
                    if (!generation.isCurrent(ticket)) return@withLock
                    controller.reportState(VpnState.Starting(profile.id))
                    runJob = scope.launch(engineDispatcher) { run(profile, settings, ticket) }
                } catch (_: IllegalArgumentException) {
                    if (generation.isCurrent(ticket)) stop(VpnState.Failed(
                        "Saved client identity is invalid. Restore it or delete and recreate the profile.",
                    ))
                } catch (_: Exception) {
                    if (generation.isCurrent(ticket)) stop(VpnState.Failed("Could not load the profile. Open it and save again."))
                }
            }
        }
        return START_STICKY
    }

    override fun onRevoke() {
        val ticket = generation.replace()
        preparation?.cancel()
        runJob?.cancel()
        scope.launch(engineDispatcher) {
            transitions.withLock { if (generation.isCurrent(ticket)) stop(VpnState.Disconnected) }
        }
    }

    override fun onDestroy() {
        generation.replace()
        preparation?.cancel()
        runJob?.cancel()
        getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(networkCallback)
        // No joins, HTTP release or native destruction on the UI thread.
        scope.launch(engineDispatcher) {
            transitions.withLock {
                runJob?.cancelAndJoin()
                runJob = null
                stopEngine()
                closeTun()
                scope.cancel()
            }
        }
        super.onDestroy()
    }

    /** Runtime repair/rotation runs first. Only a failed worker reaches full recreation here. */
    private suspend fun run(profile: Profile, settings: AppSettings, ticket: Long) {
        var failures = 0
        try {
            while (kotlin.coroutines.coroutineContext.isActive && generation.isCurrent(ticket)) {
                try {
                    val running = start(profile, settings, ticket)
                    engine = running
                    monitor(profile, running)
                } catch (e: Exception) {
                    kotlin.coroutines.coroutineContext.ensureActive()
                    if (!generation.isCurrent(ticket)) return
                    val permanent = when (e) {
                        is RtpException.Configuration -> "Check the HTTP control endpoint, server identity and DNS settings."
                        is RtpException.Identity -> "Saved client identity is invalid. Restore it or recreate the profile."
                        is RtpException.Rejected -> "Endpoint rejected the connection. Check the password and server identity."
                        is MissingKey -> "Enter the password of ${profile.name} again"
                        is TunFailure -> "Could not establish the VPN. Grant VPN permission and reconnect."
                        else -> null
                    }
                    if (permanent != null) {
                        finish(VpnState.Failed(permanent))
                        return
                    }
                    Log.i(TAG, "Transient RTP startup failure; retrying")
                } finally {
                    stopEngine()
                }
                if (!generation.isCurrent(ticket)) return
                failures++
                controller.reportState(VpnState.Running(profile.id, TunnelState.Restarting))
                showNotification("Reconnecting to ${profile.name}…")
                delay(RESTART_DELAYS_MS[minOf(failures, RESTART_DELAYS_MS.size) - 1])
            }
        } finally {
            stopEngine()
            // The service retains the TUN across worker failures and profile changes. Only an
            // explicit stop/revoke/destroy/permanent error removes its captured routes.
        }
    }

    private class MissingKey : Exception()
    private class TunFailure : Exception()

    private suspend fun start(profile: Profile, settings: AppSettings, ticket: Long): RtpEngine {
        check(generation.isCurrent(ticket))
        val key = app.keys.load(profile.id) ?: throw MissingKey()
        val operation = try {
            createRtpPreparation(profile.engineProfileJson(), key, checkNotNull(profile.clientId), protector)
        } finally { key.fill(0) }
        preparation = operation
        val version = networkVersion.get()
        try {
            if (!generation.isCurrent(ticket)) operation.cancel()
            val address = operation.prepare()
            check(generation.isCurrent(ticket))
            if (version != networkVersion.get()) throw RtpException.Cancelled()
            val config = TunConfig(address, profile.id, settings)
            if (tunConfig != config || tun == null) {
                val replacement = establish(profile, settings, address, ticket)
                // Keep the previous interface until establish returns its replacement.
                val previous = tun
                tun = replacement
                tunConfig = config
                previous?.close()
            }
            if (!generation.isCurrent(ticket)) operation.cancel()
            val owned = ParcelFileDescriptor.dup(checkNotNull(tun).fileDescriptor)
            // start accepts ownership of this duplicate even on failure. The service reference
            // keeps routes installed while failed workers close their copies.
            val running = operation.start(owned.detachFd())
            if (!generation.isCurrent(ticket)) {
                running.stop()
                running.close()
                throw RtpException.Cancelled()
            }
            if (version != networkVersion.get()) running.networkChanged()
            return running
        } finally {
            preparation = null
            operation.discard()
            operation.close()
        }
    }

    private suspend fun establish(profile: Profile, settings: AppSettings, address: String, ticket: Long): ParcelFileDescriptor {
        val builder = Builder()
            .setSession(profile.name)
            .setMtu(tunnelMtu().toInt())
            .addAddress(address, 32)
            .addDnsServer(dnsAddress())
            .setConfigureIntent(mainActivityIntent())
        for (route in routes) {
            val (destination, prefix) = route.split('/')
            builder.addRoute(destination, prefix.toInt())
        }
        builder.addRoute("::", 0)
        builder.route(settings.routing(ownPackage = packageName))
        network?.let { builder.setUnderlyingNetworks(arrayOf(it)) }
        val began = SystemClock.elapsedRealtime()
        var replacement: ParcelFileDescriptor? = null
        try {
            // Serialize only the final installation with main-thread stop/revoke requests.
            // Route construction and native I/O stay off main. If cancellation wins while
            // returning across dispatchers, close the newly established interface here.
            withContext(Dispatchers.Main) {
                ensureActive()
                if (!generation.isCurrent(ticket)) throw CancellationException()
                replacement = builder.establish() ?: throw TunFailure()
            }
        } catch (e: Exception) {
            replacement?.close()
            throw e
        }
        Log.i(TAG, "Established the assigned-address TUN with ${routes.size} routes in ${SystemClock.elapsedRealtime() - began} ms")
        return checkNotNull(replacement)
    }

    /** Reports the engine's status every second until it fails or the job is cancelled. */
    private suspend fun monitor(profile: Profile, engine: RtpEngine) {
        var shown: TunnelState? = null
        while (kotlin.coroutines.coroutineContext.isActive) {
            val status = engine.status()
            val tunnel = when (status.state) {
                RtpState.CONNECTING -> TunnelState.Connecting
                RtpState.CONNECTED -> TunnelState.Connected
                RtpState.REPAIRING -> TunnelState.Repairing
                RtpState.DEGRADED -> TunnelState.Degraded
                RtpState.FAILED, RtpState.STOPPED -> {
                    Log.w(TAG, "The engine stopped: ${status.error}")
                    return
                }
            }
            controller.reportState(VpnState.Running(profile.id, tunnel))
            controller.reportTraffic(Traffic(status.sentBytes.toLong(), status.receivedBytes.toLong()))
            if (tunnel != shown) {
                shown = tunnel
                showNotification(
                    when (tunnel) {
                        TunnelState.Connected -> "Connected to ${profile.name}"
                        TunnelState.Degraded -> "${profile.name} is not answering"
                        else -> "Connecting to ${profile.name}…"
                    },
                )
            }
            delay(1_000)
        }
    }

    /** Routes the apps of [routing] into the VPN, skipping those no longer installed. */
    private fun Builder.route(routing: AppRouting): Builder = apply {
        fun tryAdd(add: () -> Unit): Boolean = try {
            add()
            true
        } catch (_: PackageManager.NameNotFoundException) {
            false
        }

        when (routing) {
            is AppRouting.Only -> {
                val added = routing.packages.count { tryAdd { addAllowedApplication(it) } }
                // With none of them installed the system would route every app, this one too.
                if (added == 0) addDisallowedApplication(packageName)
            }
            is AppRouting.Except -> routing.packages.forEach { tryAdd { addDisallowedApplication(it) } }
        }
    }

    private suspend fun stop(state: VpnState) {
        runJob?.cancelAndJoin()
        runJob = null
        finish(state)
    }

    private fun finish(state: VpnState) {
        stopEngine()
        closeTun()
        controller.reportState(state)
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun closeTun() {
        tun?.close()
        tun = null
        tunConfig = null
    }

    private fun stopEngine() {
        engine?.let {
            it.stop()
            it.close()
        }
        engine = null
    }

    /** Shows the foreground notification, or updates it. */
    private fun showNotification(text: String) {
        val channel = NotificationChannelCompat.Builder(CHANNEL_ID, NotificationManager.IMPORTANCE_LOW)
            .setName("VPN status")
            .build()
        NotificationManagerCompat.from(this).createNotificationChannel(channel)

        val stop = PendingIntent.getService(
            this, 0, stopIntent(this), PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(text)
            .setSmallIcon(R.drawable.ic_vpn_key)
            .setContentIntent(mainActivityIntent())
            .addAction(R.drawable.baseline_stop_24, "Disconnect", stop)
            .setOngoing(true)
            .setSilent(true)
            .setForegroundServiceBehavior(NotificationCompat.FOREGROUND_SERVICE_IMMEDIATE)
            .build()
        ServiceCompat.startForeground(
            this,
            NOTIFICATION_ID,
            notification,
            ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE,
        )
    }

    private fun mainActivityIntent(): PendingIntent = PendingIntent.getActivity(
        this,
        0,
        Intent(this, MainActivity::class.java),
        PendingIntent.FLAG_IMMUTABLE,
    )

    companion object {
        private const val TAG = "RtpVpnService"
        private const val ACTION_START = "dev.fanchao.cpxy.rtp.START"
        private const val ACTION_STOP = "dev.fanchao.cpxy.rtp.STOP"
        private const val EXTRA_PROFILE_ID = "profile_id"
        private const val CHANNEL_ID = "vpn"
        private const val NOTIFICATION_ID = 1
        private val RESTART_DELAYS_MS = longArrayOf(1_000, 2_000, 4_000, 8_000, 16_000, 30_000)

        /** The same for every profile, and slow to compute: about 2,000 prefixes. */
        private val routes: List<String> by lazy { tunnelRoutes() }

        fun startIntent(context: Context, profileId: String): Intent =
            Intent(context, RtpVpnService::class.java)
                .setAction(ACTION_START)
                .putExtra(EXTRA_PROFILE_ID, profileId)

        fun stopIntent(context: Context): Intent =
            Intent(context, RtpVpnService::class.java).setAction(ACTION_STOP)
    }
}
