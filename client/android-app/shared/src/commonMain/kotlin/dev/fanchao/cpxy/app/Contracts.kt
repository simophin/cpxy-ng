package dev.fanchao.cpxy.app

interface NativeClient {
    fun start(config: NativeClientConfig): NativeClientSession
}

interface NativeClientSession {
    fun close()
}

data class NativeClientConfig(
    val httpProxyPort: UShort,
    val socks5ProxyPort: UShort,
    val apiServerPort: UShort,
    val dnsServer: String,
    val mainServerUrl: String,
    val aiServerUrl: String?,
    val tailscaleServerUrl: String?,
)

interface AppLogger {
    fun debug(tag: String, message: String)
    fun error(tag: String, message: String, throwable: Throwable? = null)
}
