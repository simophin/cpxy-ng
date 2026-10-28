package dev.fanchao.cpxy.rtp

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json

/** An RTP endpoint and the DNS servers to use with it. Its key is kept by the [KeyVault]. */
@Serializable
data class Profile(
    val id: String,
    val name: String,
    /** `host:port` of the endpoint's HTTP TCP control listener. */
    val endpoint: String,
    val serverIdentity: String = DefaultServerIdentity,
    /** Missing only on legacy profiles; provisioned durably by the repository before allocation. */
    val clientId: String? = null,
    /** Local-region resolvers, asked directly. */
    val dnsUpstream: List<String> = DefaultDnsUpstream,
    /** IPv4 resolvers asked through the tunnel. */
    val dnsAlternative: List<String> = DefaultDnsAlternative,
) {
    companion object {
        const val DefaultServerIdentity = "cpxy-rtp"
        val DefaultDnsUpstream = listOf("223.5.5.5")
        val DefaultDnsAlternative = listOf("8.8.8.8", "1.1.1.1")
    }
}

/** The engine's profile, mirroring `rtp-mobile/src/profile.rs`, which rejects unknown fields. */
@Serializable
private data class EngineProfile(
    val endpoint: String,
    @SerialName("server_identity") val serverIdentity: String,
    @SerialName("dns_upstream") val dnsUpstream: List<String>,
    @SerialName("dns_alternative") val dnsAlternative: List<String>,
)

fun Profile.engineProfileJson(): String = Json.encodeToString(
    EngineProfile(endpoint.trim(), serverIdentity, dnsUpstream, dnsAlternative),
)

/**
 * What is wrong with the profile, by field. The engine checks the endpoint and DNS server formats
 * itself (`validateProfile`).
 */
fun Profile.validationErrors(): Map<ProfileField, String> = buildMap {
    if (name.isBlank()) put(ProfileField.Name, "Name must not be blank")
    if (endpoint.isBlank()) put(ProfileField.Endpoint, "Endpoint must not be blank")
    if (serverIdentity.isEmpty() || serverIdentity.encodeToByteArray().size > 255) {
        put(ProfileField.ServerIdentity, "Must be 1 to 255 UTF-8 bytes, matching the endpoint")
    }
    if (dnsUpstream.isEmpty()) put(ProfileField.DnsUpstream, "At least one server is required")
    when {
        dnsAlternative.isEmpty() -> put(ProfileField.DnsAlternative, "At least one server is required")
        !dnsAlternative.all(::isIpv4) -> put(ProfileField.DnsAlternative, "Must be IPv4 addresses")
    }
}

enum class ProfileField { Name, Endpoint, ServerIdentity, Password, DnsUpstream, DnsAlternative }

/** The shortest password the endpoint accepts. */
const val MinPasswordLength = 12

/** Splits a text field with one DNS server per line. */
fun parseServerList(text: String): List<String> =
    text.lines().map(String::trim).filter(String::isNotEmpty)

fun isIpv4(value: String): Boolean {
    val octets = value.split('.')
    return octets.size == 4 && octets.all {
        it.isNotEmpty() && it.length <= 3 && it.all { char -> char in '0'..'9' } &&
            (it.length == 1 || it[0] != '0') && it.toInt() in 0..255
    }
}
