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
import android.os.TransactionTooLargeException
import android.util.Log
import java.net.InetAddress
import java.util.concurrent.ConcurrentHashMap
import androidx.core.app.NotificationChannelCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import dev.fanchao.cpxy.vpn.engine.Engine
import dev.fanchao.cpxy.vpn.engine.EngineException
import dev.fanchao.cpxy.vpn.engine.startEngine
import dev.fanchao.cpxy.vpn.engine.androidRoutePlan
import dev.fanchao.cpxy.vpn.engine.AndroidRoutePlan
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
    @Volatile private var latestStartId = 0
    private var finalState: VpnState = VpnState.Disconnected
    /** Reports the traffic. */
    private var monitorJob: Job? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        latestStartId = startId
        if (intent?.action == ACTION_STOP) {
            scope.launch(engineDispatcher) { stop(VpnState.Disconnected, startId) }
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
                stop(VpnState.Failed("Add a profile to connect"), startId)
                return@launch
            }
            if (startId != latestStartId) return@launch
            controller.reportState(VpnState.Connecting(profile.id))
            start(profile, app.settingsRepository.settings.first(), startId)
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
        controller.reportState(finalState)
        super.onDestroy()
    }

    private fun CoroutineScope.start(profile: Profile, settings: AppSettings, startId: Int) {
        val started = runCatching {
            val routeStarted = SystemClock.elapsedRealtime()
            val exclusions = Build.VERSION.SDK_INT >= 33
            val routeKey = profile.ipv6.name + ":" + settings.proxyRoutes.joinToString(",")
            val exactAttempt = routeKey !in oversizedRoutePlans
            var plan = androidRoutePlan(settings.proxyRoutes, profile.ipv6.name.lowercase(), exclusions, if (exactAttempt) 100_000u else 512u)
            if (!exactAttempt) Log.w(TAG, "Previously rejected exact plan; ${plan.fallbackReason}")
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
            var installedExclusions = 0
            var fullCaptureFallback = false
            fun install(routePlan: AndroidRoutePlan): android.os.ParcelFileDescriptor {
                installedExclusions = 0
                return builder().apply {
                    routePlan.included.forEach { prefix ->
                        addRoute(prefix.substringBefore('/'), prefix.substringAfter('/').toInt())
                    }
                    if (Build.VERSION.SDK_INT >= 33) {
                        routePlan.excluded.forEach {
                            val address = InetAddress.getByName(it.substringBefore('/'))
                            // Android rejects loopback prefixes. They already use the local
                            // routing table before VPN policy and need no destination exclusion.
                            if (!address.isLoopbackAddress) {
                                excludeRoute(IpPrefix(address, it.substringAfter('/').toInt()))
                                installedExclusions++
                            }
                        }
                    }
                }.establish() ?: error("The VPN permission was revoked")
            }
            val tun = try {
                install(plan)
            } catch (exactError: Exception) {
                if (exactAttempt && generateSequence<Throwable>(exactError) { it.cause }.any { it is TransactionTooLargeException }) {
                    oversizedRoutePlans.add(routeKey)
                }
                Log.w(TAG, "${if (exactAttempt) "Exact" else "Bounded"} route installation failed: included=${plan.included.size}, excluded=${plan.excluded.size}, ${exactError.javaClass.simpleName}: ${exactError.message}")
                plan = androidRoutePlan(settings.proxyRoutes, profile.ipv6.name.lowercase(), exclusions, 512u)
                Log.w(TAG, plan.fallbackReason)
                try {
                    install(plan)
                } catch (boundedError: Exception) {
                    // Full capture remains safe: the userspace router retains direct CN policy.
                    fullCaptureFallback = true
                    installedExclusions = 0
                    Log.w(TAG, "Bounded route installation failed; using full capture (${boundedError.javaClass.simpleName}: ${boundedError.message})")
                    builder().addRoute("0.0.0.0", 0).addRoute("::", 0).establish()
                        ?: error("The VPN permission was revoked")
                }
            }
            Log.i(TAG, "VPN routes: API=${Build.VERSION.SDK_INT}, included=${if (fullCaptureFallback) 2 else plan.included.size}, excluded=$installedExclusions, fullCaptureFallback=$fullCaptureFallback, setupMs=${SystemClock.elapsedRealtime() - routeStarted}")
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
            stop(VpnState.Failed((e as? EngineException.Failed)?.reason ?: e.message ?: e.toString()), startId)
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

    private fun stop(state: VpnState, startId: Int = latestStartId) {
        if (startId != latestStartId) return
        stopEngine()
        // A newer connect may arrive while native shutdown blocks. Do not stop its service.
        if (startId != latestStartId) return
        finalState = state
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        stopSelfResult(startId)
        // Publish the terminal state in onDestroy, after the service is ready to restart.
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
        // Policy-specific and process-local: IPv6 Off or changed forced routes may fit exactly.
        private val oversizedRoutePlans = ConcurrentHashMap.newKeySet<String>()
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
