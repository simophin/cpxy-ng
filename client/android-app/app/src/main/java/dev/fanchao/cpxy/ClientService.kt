package dev.fanchao.cpxy

import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Intent
import android.os.IBinder
import androidx.core.app.NotificationChannelCompat
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import dev.fanchao.cpxy.App.Companion.appInstance
import kotlinx.coroutines.DelicateCoroutinesApi
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.GlobalScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

class ClientService : Service() {
    private var startedJob: Job? = null

    override fun onBind(p0: Intent?): IBinder? = null

    @OptIn(DelicateCoroutinesApi::class)
    override fun onStartCommand(intent: Intent, flags: Int, startId: Int): Int {
        when (intent.action) {
            ACTION_START -> {
                if (startedJob != null) {
                    // Already started
                    return START_STICKY
                }

                appInstance.clientInstanceManager.start()

                val channel = NotificationChannelCompat.Builder(
                    "ongoing",
                    NotificationManager.IMPORTANCE_LOW
                ).setName("Ongoing notification")
                    .build()

                NotificationManagerCompat.from(this).createNotificationChannel(channel)

                startedJob = GlobalScope.launch(Dispatchers.Main) {
                    appInstance.clientInstanceManager.state
                        .map { it.size }
                        .distinctUntilChanged()
                        .collectLatest { num ->
                            startForeground(
                                NOTIFICATION_ID,
                                NotificationCompat.Builder(this@ClientService, channel.id)
                                    .setContentTitle(getString(R.string.app_name))
                                    .setContentText("Running $num instance(s)")
                                    .setSmallIcon(R.drawable.ic_launcher_foreground)
                                    .addAction(R.drawable.baseline_stop_24, "STOP", PendingIntent.getService(
                                        this@ClientService, 0, Intent(ACTION_STOP), PendingIntent.FLAG_IMMUTABLE
                                    ))
                                    .setOngoing(true)
                                    .build()
                            )
                        }
                }

                return START_STICKY
            }

            ACTION_STOP -> {
                startedJob?.cancel()
                startedJob = null

                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
            }
        }

        return super.onStartCommand(intent, flags, startId)
    }

    companion object {
        private const val NOTIFICATION_ID = 1

        const val ACTION_START = "start"
        const val ACTION_STOP = "stop"
    }
}