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
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (startedJob != null) {
            // Already started
            return super.onStartCommand(intent, flags, startId)
        }

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
                            .addAction(R.drawable.baseline_stop_24, "STOP", PendingIntent.getBroadcast(
                                this@ClientService, 0, Intent(this@ClientService,
                                    StopServiceReceiver::class.java), PendingIntent.FLAG_IMMUTABLE
                            ))
                            .setContentIntent(PendingIntent.getActivity(
                                this@ClientService,
                                1,
                                Intent(this@ClientService, MainActivity::class.java)
                                    .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP),
                                PendingIntent.FLAG_IMMUTABLE
                            ))
                            .setSilent(true)
                            .setOngoing(true)
                            .build()
                    )
                }
        }

        return START_STICKY
    }

    override fun onDestroy() {
        super.onDestroy()

        startedJob?.cancel()
        startedJob = null
    }

    companion object {
        private const val NOTIFICATION_ID = 1

    }
}