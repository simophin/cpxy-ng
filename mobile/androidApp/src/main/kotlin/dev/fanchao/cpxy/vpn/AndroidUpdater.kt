package dev.fanchao.cpxy.vpn

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.PackageInstaller
import android.net.VpnService
import androidx.core.content.ContextCompat
import androidx.core.content.IntentCompat
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.File
import java.io.IOException
import java.net.HttpURLConnection
import java.net.URI
import java.net.URL
import java.security.MessageDigest

/**
 * Updates from the download server ([baseUrl], download-worker/). `latest/…` redirects to the
 * newest complete version folder, which holds the APK and a SHA256SUMS of everything in it.
 */
class AndroidUpdater(private val context: Context, private val baseUrl: String) : Updater {
    override suspend fun latestVersion(): String? = withContext(Dispatchers.IO) {
        val connection = open("$baseUrl/latest/$APK_PATH").apply {
            requestMethod = "HEAD"
            instanceFollowRedirects = false
        }
        try {
            when (val code = connection.responseCode) {
                HttpURLConnection.HTTP_NOT_FOUND -> null
                in 300..399 -> {
                    val location = connection.getHeaderField("Location") ?: throw IOException("The server sent no version")
                    // /<version>/android/cpxy-vpn.apk
                    URI(baseUrl).resolve(location).path.removeSuffix("/$APK_PATH").substringAfterLast('/')
                }
                else -> throw IOException("The server answered $code")
            }
        } finally {
            connection.disconnect()
        }
    }

    override suspend fun install(versionName: String, onProgress: (Float?) -> Unit): Unit = withContext(Dispatchers.IO) {
        val folder = "$baseUrl/$versionName"
        val expected = open("$folder/SHA256SUMS").let { connection ->
            try {
                check(connection.responseCode == HttpURLConnection.HTTP_OK) { "Version $versionName is not published" }
                connection.inputStream.bufferedReader().readLines()
            } finally {
                connection.disconnect()
            }
        }.firstNotNullOfOrNull { line ->
            line.split(Regex("\\s+\\*?"), limit = 2).takeIf { it.size == 2 && it[1] == APK_PATH }?.get(0)
        } ?: throw IOException("The checksums do not list the app")

        val apk = File(context.cacheDir, "update.apk")
        val digest = MessageDigest.getInstance("SHA-256")
        val connection = open("$folder/$APK_PATH")
        try {
            check(connection.responseCode == HttpURLConnection.HTTP_OK) { "The download failed: ${connection.responseCode}" }
            val size = connection.contentLengthLong.takeIf { it > 0 }
            connection.inputStream.use { input ->
                apk.outputStream().use { output ->
                    val buffer = ByteArray(64 * 1024)
                    var done = 0L
                    while (true) {
                        val read = input.read(buffer)
                        if (read < 0) break
                        output.write(buffer, 0, read)
                        digest.update(buffer, 0, read)
                        done += read
                        onProgress(size?.let { done.toFloat() / it })
                    }
                }
            }
        } finally {
            connection.disconnect()
        }
        val actual = digest.digest().joinToString("") { "%02x".format(it) }
        if (!actual.equals(expected, ignoreCase = true)) {
            apk.delete()
            throw IOException("The download is damaged; try again")
        }

        // Reconnect once the new version is installed, unless the always-on VPN does that.
        val state = context.app.controller.state.value
        updatePreferences(context).edit()
            .putBoolean(KEY_RECONNECT, state is VpnState.Connected || state is VpnState.Connecting)
            .apply()

        val installer = context.packageManager.packageInstaller
        val params = PackageInstaller.SessionParams(PackageInstaller.SessionParams.MODE_FULL_INSTALL)
            .apply { setAppPackageName(context.packageName) }
        val sessionId = installer.createSession(params)
        installer.openSession(sessionId).use { session ->
            session.openWrite("cpxy-vpn.apk", 0, apk.length()).use { output ->
                apk.inputStream().use { it.copyTo(output) }
                session.fsync(output)
            }
            val result = PendingIntent.getBroadcast(
                context,
                sessionId,
                Intent(context, UpdateInstallReceiver::class.java),
                // The installer adds its status to the intent
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
            )
            session.commit(result.intentSender)
        }
        apk.delete()
    }

    private fun open(url: String): HttpURLConnection =
        (URL(url).openConnection() as HttpURLConnection).apply {
            connectTimeout = 15_000
            readTimeout = 30_000
            setRequestProperty("Cache-Control", "no-cache")
        }

    companion object {
        const val APK_PATH = "android/cpxy-vpn.apk"
        internal const val KEY_RECONNECT = "reconnect_after_update"

        internal fun updatePreferences(context: Context) =
            context.getSharedPreferences("updates", Context.MODE_PRIVATE)
    }
}

/** What the system installer did with the update. */
class UpdateInstallReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        when (val status = intent.getIntExtra(PackageInstaller.EXTRA_STATUS, PackageInstaller.STATUS_FAILURE)) {
            PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                val confirm = IntentCompat.getParcelableExtra(intent, Intent.EXTRA_INTENT, Intent::class.java) ?: return
                context.startActivity(confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
            }
            // The app is replaced, and restarted by UpdatedReceiver
            PackageInstaller.STATUS_SUCCESS -> Unit
            else -> {
                AndroidUpdater.updatePreferences(context).edit().remove(AndroidUpdater.KEY_RECONNECT).apply()
                val message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE)
                context.app.updates?.installFailed(
                    if (status == PackageInstaller.STATUS_FAILURE_ABORTED) "The update was cancelled"
                    else "The update was not installed${message?.let { ": $it" } ?: ""}",
                )
            }
        }
    }
}

/** Reconnects the VPN after an update replaced the app that was running it. */
class UpdatedReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intent.ACTION_MY_PACKAGE_REPLACED) return
        val preferences = AndroidUpdater.updatePreferences(context)
        if (!preferences.getBoolean(AndroidUpdater.KEY_RECONNECT, false)) return
        preferences.edit().remove(AndroidUpdater.KEY_RECONNECT).apply()
        // Still allowed to run the VPN, so no consent is needed
        if (VpnService.prepare(context) == null) {
            ContextCompat.startForegroundService(context, CpxyVpnService.resumeIntent(context))
        }
    }
}
