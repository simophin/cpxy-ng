package dev.fanchao.cpxy.vpn

import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.VpnService
import android.util.Log
import androidx.core.app.NotificationChannelCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.ServiceCompat
import dev.fanchao.cpxy.vpn.engine.Engine
import dev.fanchao.cpxy.vpn.engine.EngineException
import dev.fanchao.cpxy.vpn.engine.EngineListener
import dev.fanchao.cpxy.vpn.engine.startEngine
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.coroutines.runBlocking
import dev.fanchao.cpxy.vpn.engine.ConnectionEvent as EngineConnectionEvent

/**
 * Sets up the TUN device and runs the engine on it. The app itself is excluded from the VPN, so
 * the engine's own sockets bypass the tunnel.
 */
class CpxyVpnService : VpnService() {
    private val controller get() = app.controller
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)

    // The engine calls block, so they run one at a time off the main thread. Only touch
    // `engine` and `monitorJob` from here.
    @OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
    private val engineDispatcher = Dispatchers.IO.limitedParallelism(1)
    private var engine: Engine? = null
    /** Reports the traffic, and turns the connection events on while the UI wants them. */
    private var monitorJob: Job? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_START) {
            val profileId = checkNotNull(intent.getStringExtra(EXTRA_PROFILE_ID))
            val profileName = checkNotNull(intent.getStringExtra(EXTRA_PROFILE_NAME))
            val config = checkNotNull(intent.getStringExtra(EXTRA_CONFIG))
            showNotification(profileName)
            scope.launch(engineDispatcher) { start(profileId, profileName, config) }
        } else {
            scope.launch(engineDispatcher) { stop(VpnState.Disconnected) }
        }
        return START_NOT_STICKY
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

    private fun CoroutineScope.start(profileId: String, profileName: String, config: String) {
        stopEngine()
        val started = runCatching {
            val tun = Builder()
                .setSession(profileName)
                .setMtu(TunParameters.MTU)
                .addAddress(TunParameters.ADDRESS, TunParameters.PREFIX_LENGTH)
                .addDnsServer(TunParameters.DNS_SERVER)
                .addRoute("0.0.0.0", 0)
                // No IPv6 address, so apps do not use IPv6; the route keeps it off the
                // underlying network.
                .addRoute("::", 0)
                .addDisallowedApplication(packageName)
                .setConfigureIntent(mainActivityIntent())
                .establish()
                ?: error("The VPN permission was revoked")
            // The engine owns the descriptor from here, and closes it even when it fails to start.
            startEngine(tun.detachFd(), config, Listener(controller))
        }

        started.onSuccess { started ->
            engine = started
            controller.reportState(VpnState.Connected(profileId))
            monitorJob = launch {
                launch {
                    controller.connectionsWanted.collect(started::setEventsEnabled)
                }
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

    private fun stop(state: VpnState) {
        stopEngine()
        controller.reportState(state)
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
        stopSelf()
    }

    private fun stopEngine() {
        monitorJob?.cancel()
        monitorJob = null
        engine?.let {
            it.stop()
            it.close()
        }
        engine = null
    }

    private fun showNotification(profileName: String) {
        val channel = NotificationChannelCompat.Builder(CHANNEL_ID, NotificationManager.IMPORTANCE_LOW)
            .setName("VPN status")
            .build()
        NotificationManagerCompat.from(this).createNotificationChannel(channel)

        val stop = PendingIntent.getService(
            this, 0, stopIntent(this), PendingIntent.FLAG_IMMUTABLE,
        )
        val notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle(getString(R.string.app_name))
            .setContentText("Connected to $profileName")
            .setSmallIcon(R.drawable.ic_launcher_foreground)
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

    /** Called from engine threads. */
    private class Listener(private val controller: AndroidVpnController) : EngineListener {
        override fun onConnection(event: EngineConnectionEvent) {
            controller.reportConnection(
                ConnectionEvent(
                    host = event.host,
                    port = event.port.toInt(),
                    outbound = event.outbound,
                    delayMillis = event.delayMillis.toLong(),
                    timeMillis = event.timeMillis.toLong(),
                    error = event.error,
                    countryCode = event.countryCode,
                )
            )
        }
    }

    companion object {
        private const val TAG = "CpxyVpnService"
        private const val ACTION_START = "dev.fanchao.cpxy.vpn.START"
        private const val ACTION_STOP = "dev.fanchao.cpxy.vpn.STOP"
        private const val EXTRA_PROFILE_ID = "profile_id"
        private const val EXTRA_PROFILE_NAME = "profile_name"
        private const val EXTRA_CONFIG = "config"
        private const val CHANNEL_ID = "vpn"
        private const val NOTIFICATION_ID = 1

        fun startIntent(context: Context, profile: Profile): Intent =
            Intent(context, CpxyVpnService::class.java)
                .setAction(ACTION_START)
                .putExtra(EXTRA_PROFILE_ID, profile.id)
                .putExtra(EXTRA_PROFILE_NAME, profile.name)
                .putExtra(EXTRA_CONFIG, profile.engineConfigJson())

        fun stopIntent(context: Context): Intent =
            Intent(context, CpxyVpnService::class.java).setAction(ACTION_STOP)
    }
}
