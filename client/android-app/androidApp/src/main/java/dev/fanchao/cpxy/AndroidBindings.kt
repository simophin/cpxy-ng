package dev.fanchao.cpxy

import android.content.Context
import android.util.Log
import com.sun.jna.Native
import dev.fanchao.cpxy.app.AppLogger
import dev.fanchao.cpxy.app.AppScope
import dev.fanchao.cpxy.app.ConfigPersistence
import dev.fanchao.cpxy.app.NativeClient
import dev.fanchao.cpxy.app.NativeClientConfig
import dev.fanchao.cpxy.app.NativeClientSession
import dev.zacsweers.metro.ContributesBinding
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn

@Inject
@ContributesBinding(AppScope::class)
@SingleIn(AppScope::class)
class SharedPreferencesConfigPersistence(context: Context) : ConfigPersistence {
    private val preferences = context.getSharedPreferences("default", Context.MODE_PRIVATE)
    override fun load(): String? = preferences.getString(PREF_KEY, null)
    override fun save(value: String) = preferences.edit().putString(PREF_KEY, value).apply()

    private companion object { const val PREF_KEY = "config" }
}

@Inject
@ContributesBinding(AppScope::class)
@SingleIn(AppScope::class)
class AndroidNativeClient : NativeClient {
    private val client: Client by lazy { Native.load("client", Client::class.java) as Client }

    override fun start(config: NativeClientConfig): NativeClientSession = client.create(
        httpProxyPort = config.httpProxyPort,
        socks5ProxyPort = config.socks5ProxyPort,
        apiServerPort = config.apiServerPort,
        dnsServer = config.dnsServer,
        mainServerUrl = config.mainServerUrl,
        aiServerUrl = config.aiServerUrl,
        tailscaleServerUrl = config.tailscaleServerUrl,
    )
}

@Inject
@ContributesBinding(AppScope::class)
@SingleIn(AppScope::class)
class AndroidAppLogger : AppLogger {
    override fun debug(tag: String, message: String) = Log.d(tag, message).let { Unit }
    override fun error(tag: String, message: String, throwable: Throwable?) {
        Log.e(tag, message, throwable)
    }
}
