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
    val server: String,
    /** DNS servers whose answer is used when all its addresses are in CN. */
    val dnsUpstream: List<String>,
    /** DNS servers whose answer is used otherwise. */
    val dnsAlternative: List<String>,
)

/**
 * The engine configuration, mirroring `mobile-engine/src/config.rs`. The engine rejects unknown
 * fields, so keep the two in step.
 */
@Serializable
data class EngineConfig(
    val server: String,
    @SerialName("dns_upstream") val dnsUpstream: List<String>,
    @SerialName("dns_alternative") val dnsAlternative: List<String>,
    val mtu: Int? = null,
)

/** The TUN parameters the platform sets up, mirroring `mobile-engine/src/config.rs`. */
object TunParameters {
    const val ADDRESS = "10.233.0.1"
    const val PREFIX_LENGTH = 30

    /** Only exists inside the engine, which answers it with the DNS split. */
    const val DNS_SERVER = "10.233.0.2"
    const val MTU = 1500
}

private val engineJson = Json { explicitNulls = false }

fun Profile.engineConfigJson(): String = engineJson.encodeToString(
    EngineConfig(
        server = server.trim(),
        dnsUpstream = dnsUpstream,
        dnsAlternative = dnsAlternative,
        mtu = TunParameters.MTU,
    )
)

/**
 * What is wrong with the profile, by field. The engine checks the URL and DNS server formats
 * itself and fails to start with a message.
 */
fun Profile.validationErrors(): Map<ProfileField, String> = buildMap {
    if (name.isBlank()) put(ProfileField.Name, "Name must not be blank")
    if (server.isBlank()) put(ProfileField.Server, "Server must not be blank")
    if (dnsUpstream.isEmpty()) put(ProfileField.DnsUpstream, "At least one server is required")
    if (dnsAlternative.isEmpty()) put(ProfileField.DnsAlternative, "At least one server is required")
}

enum class ProfileField { Name, Server, DnsUpstream, DnsAlternative }

/** Splits a text field with one DNS server per line. */
fun parseServerList(text: String): List<String> =
    text.lines().map(String::trim).filter(String::isNotEmpty)
