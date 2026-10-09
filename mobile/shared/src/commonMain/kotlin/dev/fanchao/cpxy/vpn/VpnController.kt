package dev.fanchao.cpxy.vpn

import kotlinx.coroutines.flow.StateFlow

sealed interface VpnState {
    data object Disconnected : VpnState
    data class Connecting(val profileId: String) : VpnState
    data class Connected(val profileId: String) : VpnState
    data class Failed(val message: String) : VpnState
}

data class Traffic(
    /** IP bytes from the device into the tunnel. */
    val sent: Long = 0,
    /** IP bytes from the tunnel to the device. */
    val received: Long = 0,
)

/** A TCP flow the engine connected direct or through the proxy, or failed to. */
data class ConnectionEvent(
    val host: String,
    val port: Int,
    /** `direct` or `proxy`. */
    val outbound: String,
    val delayMillis: Long,
    val timeMillis: Long,
    val error: String?,
)

/** Starts and stops the VPN on the platform. */
interface VpnController {
    val state: StateFlow<VpnState>
    val traffic: StateFlow<Traffic>

    /** The most recent connections, newest first. */
    val connections: StateFlow<List<ConnectionEvent>>

    /** Asks for the VPN permission if needed, then connects. */
    suspend fun connect(profile: Profile)

    fun disconnect()
}
