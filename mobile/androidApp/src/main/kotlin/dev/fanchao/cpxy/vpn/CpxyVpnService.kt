package dev.fanchao.cpxy.vpn

import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.content.pm.ServiceInfo
import android.net.VpnService
import android.net.IpPrefix
import android.os.Build
import android.os.SystemClock
import android.util.Log
import java.net.InetAddress
import androidx.core.app.NotificationChannelCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import dev.fanchao.cpxy.vpn.engine.Engine
import dev.fanchao.cpxy.vpn.engine.EngineException
import dev.fanchao.cpxy.vpn.engine.startEngine
import dev.fanchao.cpxy.vpn.engine.androidRoutePlan
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.Json

/**
 * Sets up the TUN device and runs the engine on it. The app itself is excluded from the VPN, so
 * the engine's own sockets bypass the tunnel. The system starts it too, for the always-on VPN.
 */
class CpxyVpnService : VpnService() {
    private val controller get() = app.controller
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    // The engine calls block, so they run one at a time off the main thread. Only touch
    // `engine` and `monitorJob` from here.
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    private val engineDispatcher = Dispatchers.IO.limitedParallelism(1)
    private var engine: Engine? = null
    /** Reports the traffic. */
    private var monitorJob: Job? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            scope.launch(engineDispatcher) { stop(VpnState.Disconnected) }
            return START_NOT_STICKY
        }

        // Started by the app with a profile. Otherwise uses the selected one: started by the
        // system for the always-on VPN (`VpnService.SERVICE_INTERFACE`), or restarted with no
        // intent after the process was killed while connected.
        val requested = intent?.getStringExtra(EXTRA_PROFILE)?.let { Json.decodeFromString<Profile>(it) }
        showNotification(requested?.let { "Connecting to ${it.name}…" } ?: "Connecting…")
        scope.launch(engineDispatcher) {
            val profile = requested ?: app.repository.profiles.first().selected
            if (profile == null) {
                stop(VpnState.Failed("Add a profile to connect"))
                return@launch
            }
            controller.reportState(VpnState.Connecting(profile.id))
            start(profile, app.settingsRepository.settings.first())
        }
        return START_STICKY
    }

    override fun onRevoke() {
        // Another VPN took over, or the user turned this one off in the settings.
        scope.launch(engineDispatcher) { stop(VpnState.Disconnected) }
    }

    override fun onDestroy() {
        runBlocking(engineDispatcher) { stopEngine() }
        scope.cancel()
        super.onDestroy()
    }

    private fun CoroutineScope.start(profile: Profile, settings: AppSettings) {
        val started = runCatching {
            val routeStarted = SystemClock.elapsedRealtime()
            val exclusions = Build.VERSION.SDK_INT >= 33
            val plan = androidRoutePlan(settings.proxyRoutes, profile.ipv6.name.lowercase(), exclusions)
            if (plan.fallbackReason.isNotEmpty()) Log.w(TAG, plan.fallbackReason)
            fun builder() = Builder()
                .setSession(profile.name)
                .setMtu(TunParameters.MTU)
                .addAddress(TunParameters.ADDRESS, TunParameters.PREFIX_LENGTH)
                .addDnsServer(TunParameters.DNS_SERVER)
                .apply { settings.dnsSearchDomains.forEach { addSearchDomain(it) } }
                // Without an IPv6 address apps do not use IPv6; the route keeps it off the
                // underlying network either way. On a network without IPv6, the engine refuses
                // what would go direct, so apps fall back to IPv4.
                .apply {
                    if (profile.ipv6 != Ipv6Mode.Off) {
                        addAddress(TunParameters.ADDRESS_V6, TunParameters.PREFIX_LENGTH_V6)
                    }
                }
                .route(settings.routing(ownPackage = packageName))
                .setConfigureIntent(mainActivityIntent())
            var fullCaptureFallback = false
            val tun = try {
                builder().apply {
                    plan.included.forEach { prefix ->
                        addRoute(prefix.substringBefore('/'), prefix.substringAfter('/').toInt())
                    }
                    if (Build.VERSION.SDK_INT >= 33) {
                        plan.excluded.forEach {
                            excludeRoute(IpPrefix(InetAddress.getByName(it.substringBefore('/')), it.substringAfter('/').toInt()))
                        }
                    }
                }.establish() ?: error("The VPN permission was revoked")
            } catch (e: Exception) {
                // A device may impose a smaller route/Binder limit. Full capture is safe because
                // the split SOCKS router still relays captured CN/private flows directly.
                fullCaptureFallback = true
                Log.w(TAG, "Route plan installation failed; retrying with full capture (${e.javaClass.simpleName})")
                builder().addRoute("0.0.0.0", 0).addRoute("::", 0).establish()
                    ?: error("The VPN permission was revoked")
            }
            Log.i(TAG, "VPN routes: API=${Build.VERSION.SDK_INT}, included=${plan.included.size}, excluded=${plan.excluded.size}, fullCaptureFallback=$fullCaptureFallback, setupMs=${SystemClock.elapsedRealtime() - routeStarted}")
            // Establishing the replacement first keeps policy updates/profile switches from
            // briefly removing VPN capture while the old engine shuts down.
            stopEngine()
            // The engine owns the descriptor from here, and closes it even when it fails to start.
            startEngine(tun.detachFd(), profile.engineConfigJson(settings, androidSocks = true))
        }

        started.onSuccess { started ->
            engine = started
            controller.engine = started
            controller.reportState(VpnState.Connected(profile.id))
            showNotification("Connected to ${profile.name}")
            monitorJob = launch {
                while (isActive) {
                    val traffic = started.traffic()
                    controller.reportTraffic(Traffic(traffic.sent.toLong(), traffic.received.toLong()))
                    delay(1_000)
                }
            }
        }.onFailure { e ->
            Log.e(TAG, "Failed to start the engine", e)
            stop(VpnState.Failed((e as? EngineException.Failed)?.reason ?: e.message ?: e.toString()))
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

    private fun stop(state: VpnState) {
        stopEngine()
        controller.reportState(state)
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun stopEngine() {
        monitorJob?.cancel()
        monitorJob = null
        controller.engine = null
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
        private const val TAG = "CpxyVpnService"
        private const val ACTION_START = "dev.fanchao.cpxy.vpn.START"
        private const val ACTION_STOP = "dev.fanchao.cpxy.vpn.STOP"
        private const val EXTRA_PROFILE = "profile"
        private const val CHANNEL_ID = "vpn"
        private const val NOTIFICATION_ID = 1

        fun startIntent(context: Context, profile: Profile): Intent =
            Intent(context, CpxyVpnService::class.java)
                .setAction(ACTION_START)
                .putExtra(EXTRA_PROFILE, Json.encodeToString(profile))

        /** Connects with the selected profile. */
        fun resumeIntent(context: Context): Intent =
            Intent(context, CpxyVpnService::class.java).setAction(ACTION_START)

        fun stopIntent(context: Context): Intent =
            Intent(context, CpxyVpnService::class.java).setAction(ACTION_STOP)
    }
}
