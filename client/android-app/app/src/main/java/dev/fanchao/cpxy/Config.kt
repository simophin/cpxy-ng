package dev.fanchao.cpxy

import kotlinx.serialization.Serializable


@Serializable
data class Profile(
    val id: String,
    val name: String,
    val mainServerUrl: String,
    val aiServerUrl: String?,
    val tailscaleServerUrl: String?,
)

@Serializable
data class ClientConfig(
    val profiles: List<Profile>,
    val enabledProfileId: String?,
    val httpProxyPort: UShort,
    val socks5ProxyPort: UShort,
    val apiServerPort: UShort = 3010u,
) {
    val enabledProfile: Profile?
        get() = enabledProfileId?.let { id -> profiles.find { it.id == id } }
}