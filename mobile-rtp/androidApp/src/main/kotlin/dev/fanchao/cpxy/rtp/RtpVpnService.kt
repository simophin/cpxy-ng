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
import dev.fanchao.cpxy.rtp.engine.startRtpEngine
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
import kotlinx.coroutines.runBlocking

/**
 * Sets up the TUN device with the engine's routes and runs the engine on it, starting it again
 * with backoff when it fails. The TUN stays up meanwhile, so traffic for the tunnel waits rather
 * than going direct. The app itself is excluded from the VPN, so the engine's sockets bypass it.
 */
class RtpVpnService : VpnService() {
    private val controller get() = app.controller
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    // The engine calls block, so they run one at a time off the main thread. Only touch `runJob`
    // from here; `engine` is also read by the network callback.
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    private val engineDispatcher = Dispatchers.IO.limitedParallelism(1)
    private var runJob: Job? = null
    @Volatile
    private var engine: RtpEngine? = null

    /** The best non-VPN network: the one the engine's sockets use. */
    @Volatile
    private var network: Network? = null
    private val networkCallback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            val previous = this@RtpVpnService.network
            Log.d(TAG, "Underlying network $network, was $previous")
            this@RtpVpnService.network = network
            setUnderlyingNetworks(arrayOf(network))
            if (previous != null && previous != network) {
                Log.i(TAG, "The network changed; repairing the tunnel")
                engine?.networkChanged()
            }
        }
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
        if (intent?.action == ACTION_STOP) {
            scope.launch(engineDispatcher) { stop(VpnState.Disconnected) }
            return START_NOT_STICKY
        }

        // Started by the app with a profile, or restarted by the system with no intent after the
        // process was killed while connected: then uses the selected one.
        val requestedId = intent?.getStringExtra(EXTRA_PROFILE_ID)
        showNotification("Connecting…")
        scope.launch(engineDispatcher) {
            val stored = app.repository.profiles.first()
            val profile = stored.profiles.firstOrNull { it.id == requestedId } ?: stored.selected
            if (profile == null) {
                stop(VpnState.Failed("Add a profile to connect"))
                return@launch
            }
            val key = app.keys.load(profile.id)
            if (key == null) {
                stop(VpnState.Failed("Enter the password of ${profile.name} again"))
                return@launch
            }
            runJob?.cancelAndJoin()
            stopEngine()
            controller.reportState(VpnState.Starting(profile.id))
            val settings = app.settingsRepository.settings.first()
            runJob = launch { run(profile, key, settings) }
        }
        return START_STICKY
    }

    override fun onRevoke() {
        // Another VPN took over, or the user turned this one off in the settings.
        scope.launch(engineDispatcher) { stop(VpnState.Disconnected) }
    }

    override fun onDestroy() {
        getSystemService(ConnectivityManager::class.java).unregisterNetworkCallback(networkCallback)
        runBlocking(engineDispatcher) {
            runJob?.cancelAndJoin()
            stopEngine()
        }
        scope.cancel()
        super.onDestroy()
    }

    /** Runs the engine until cancelled, starting it again whenever it fails. */
    private suspend fun CoroutineScope.run(profile: Profile, key: ByteArray, settings: AppSettings) {
        var failures = 0
        while (isActive) {
            val started = runCatching { start(profile, key, settings) }
            val running = started.getOrElse { e ->
                Log.e(TAG, "Failed to start the engine", e)
                val message = (e as? RtpException.Failed)?.reason ?: e.message ?: e.toString()
                if (failures == 0) {
                    // A first start that fails is a problem with the profile, not the network.
                    stop(VpnState.Failed(message))
                    return
                }
                null
            }
            if (running != null) {
                engine = running
                monitor(profile, running)
                stopEngine()
            }
            failures++
            controller.reportState(VpnState.Running(profile.id, TunnelState.Restarting))
            showNotification("Restarting ${profile.name}…")
            delay(RESTART_DELAYS_MS[minOf(failures, RESTART_DELAYS_MS.size) - 1])
        }
    }

    private fun start(profile: Profile, key: ByteArray, settings: AppSettings): RtpEngine {
        val builder = Builder()
            .setSession(profile.name)
            .setMtu(tunnelMtu().toInt())
            .addAddress(profile.address, 32)
            .addDnsServer(dnsAddress())
            .setConfigureIntent(mainActivityIntent())
        for (route in routes) {
            val (address, prefix) = route.split('/')
            builder.addRoute(address, prefix.toInt())
        }
        // The tunnel carries IPv4 only. Without an IPv6 address apps do not use IPv6; the route
        // keeps it off the underlying network either way, and the engine refuses it.
        builder.addRoute("::", 0)
        builder.route(settings.routing(ownPackage = packageName))
        network?.let { builder.setUnderlyingNetworks(arrayOf(it)) }
        val began = SystemClock.elapsedRealtime()
        val tun = builder.establish() ?: error("The VPN permission was revoked")
        Log.i(TAG, "Established the TUN with ${routes.size} routes in ${SystemClock.elapsedRealtime() - began} ms")
        // The engine owns the descriptor from here, and closes it even when it fails to start.
        return startRtpEngine(tun.detachFd(), profile.engineProfileJson(), key, protector)
    }

    /** Reports the engine's status every second until it fails or the job is cancelled. */
    private suspend fun CoroutineScope.monitor(profile: Profile, engine: RtpEngine) {
        var shown: TunnelState? = null
        while (isActive) {
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
        runJob?.takeUnless { it == kotlin.coroutines.coroutineContext[Job] }?.cancelAndJoin()
        runJob = null
        stopEngine()
        controller.reportState(state)
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        stopSelf()
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
