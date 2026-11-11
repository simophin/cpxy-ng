package dev.fanchao.cpxy.vpn

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** A server and the DNS servers to use with it. */
@Serializable
data class Profile(
    val id: String,
    val name: String,
    /** The cpxy server URL with its key, e.g. `https://:key@example.com`. */
    val server: String = "",
    /** DNS servers whose answer is used when all its addresses are in CN. */
    val dnsUpstream: List<String>,
    /** DNS servers whose answer is used otherwise. */
    val dnsAlternative: List<String>,
    val backend: ProfileBackend = ProfileBackend.Cpxy,
    /** External no-auth SOCKS endpoint, host:port; IPv6 hosts use brackets. */
    val socks5: String = "",
)

@Serializable
enum class ProfileBackend { Cpxy, Socks5 }

/**
 * The engine configuration, mirroring `mobile-engine/src/config.rs`. The engine rejects unknown
 * fields, so keep the two in step.
 */
@Serializable
data class EngineConfig(
    val server: String,
    @SerialName("dns_upstream") val dnsUpstream: List<String>,
    @SerialName("dns_alternative") val dnsAlternative: List<String>,
    @SerialName("dns_search_domains") val dnsSearchDomains: List<String> = emptyList(),
    @SerialName("proxy_routes") val proxyRoutes: List<String> = emptyList(),
    val mtu: Int? = null,
    @SerialName("http_proxy_port") val httpProxyPort: Int? = null,
    @SerialName("socks_proxy_port") val socksProxyPort: Int? = null,
    val socks5: String? = null,
    @SerialName("android_socks") val androidSocks: Boolean = false,
)

/**
 * The TUN parameters the platform sets up, mirroring `mobile-engine/src/config.rs`. IPv4 only:
 * without an IPv6 address apps use IPv4, and the engine refuses any IPv6 that reaches it.
 */
object TunParameters {
    const val ADDRESS = "10.233.0.1"
    const val PREFIX_LENGTH = 30

    /** Only exists inside the engine, which answers it with the DNS split. */
    const val DNS_SERVER = "10.233.0.2"
    const val MTU = 1500
}

private val engineJson = Json { explicitNulls = false }

fun Profile.engineConfigJson(settings: AppSettings = AppSettings(), androidSocks: Boolean = false): String = engineJson.encodeToString(
    EngineConfig(
        server = if (backend == ProfileBackend.Cpxy) server.trim() else "",
        socks5 = socks5.trim().takeIf { backend == ProfileBackend.Socks5 },
        androidSocks = androidSocks,
        proxyRoutes = settings.proxyRoutes,
        dnsSearchDomains = settings.dnsSearchDomains,
        dnsUpstream = dnsUpstream,
        dnsAlternative = dnsAlternative,
        mtu = TunParameters.MTU,
        httpProxyPort = settings.httpProxyPort.takeIf { settings.httpProxyEnabled },
        socksProxyPort = settings.socksProxyPort.takeIf { settings.socksProxyEnabled },
    )
)

/**
 * What is wrong with the profile, by field. The engine checks the URL and DNS server formats
 * itself and fails to start with a message.
 */
fun Profile.validationErrors(): Map<ProfileField, String> = buildMap {
    if (name.isBlank()) put(ProfileField.Name, "Name must not be blank")
    if (backend == ProfileBackend.Cpxy && server.isBlank()) put(ProfileField.Server, "Server must not be blank")
    if (backend == ProfileBackend.Socks5 && !validSocksEndpoint(socks5)) {
        put(ProfileField.Server, "Use host:port or [IPv6]:port, without authentication or TLS")
    }
    if (dnsUpstream.isEmpty()) put(ProfileField.DnsUpstream, "At least one server is required")
    if (dnsAlternative.isEmpty()) put(ProfileField.DnsAlternative, "At least one server is required")
}

enum class ProfileField { Name, Server, DnsUpstream, DnsAlternative }

/** Splits a text field with one DNS server per line. */
fun parseServerList(text: String): List<String> =
    text.lines().map(String::trim).filter(String::isNotEmpty)

/** Syntax validation only. The engine resolves the host outside the VPN at startup. */
fun validSocksEndpoint(value: String): Boolean {
    if (value.isBlank() || value.any { it.isWhitespace() || it in "/@?#" }) return false
    val host = value.substringBeforeLast(':', "")
    val port = value.substringAfterLast(':', "").toIntOrNull() ?: return false
    if (port !in 1..65535 || host.isBlank()) return false
    return if (host.startsWith("[")) {
        host.endsWith("]") && host.length > 3 && host.substring(1, host.length - 1).all { it.isDigit() || it.lowercaseChar() in 'a'..'f' || it == ':' || it == '.' } && ':' in host
    } else {
        host.all { it.isLetterOrDigit() || it == '.' || it == '-' } && host.any { it.isLetterOrDigit() }
    }
}
