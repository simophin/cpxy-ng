package dev.fanchao.cpxy.vpn

import kotlinx.coroutines.flow.Flow
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
    /** ISO 3166-1 alpha-2 code of the country [host] is in, when known. */
    val countryCode: String?,
)

/** Starts and stops the VPN on the platform. */
interface VpnController {
    val state: StateFlow<VpnState>
    val traffic: StateFlow<Traffic>

    /**
     * Connections as the engine reports them, without history. The engine only reports them while
     * this is collected.
     */
    val connections: Flow<ConnectionEvent>

    /** Asks for the VPN permission if needed, then connects. */
    suspend fun connect(profile: Profile)

    fun disconnect()
}
