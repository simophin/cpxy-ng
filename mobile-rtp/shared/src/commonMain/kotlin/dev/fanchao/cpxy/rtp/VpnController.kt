package dev.fanchao.cpxy.rtp

import kotlinx.coroutines.flow.StateFlow

/** The state of the tunnel to the endpoint while the VPN is up. */
enum class TunnelState {
    /** Registering with the endpoint. */
    Connecting,
    Connected,

    /** Replacing a path that stopped answering, such as after a network change. */
    Repairing,

    /** The endpoint is not answering; traffic waits rather than going direct. */
    Degraded,

    /** The engine failed and is being started again. */
    Restarting,
}

sealed interface VpnState {
    data object Disconnected : VpnState
    data class Starting(val profileId: String) : VpnState
    data class Running(val profileId: String, val tunnel: TunnelState) : VpnState
    data class Failed(val message: String) : VpnState
}

data class Traffic(
    /** IP bytes from the device into the tunnel. */
    val sent: Long = 0,
    /** IP bytes from the tunnel to the device. */
    val received: Long = 0,
)

/** Starts and stops the VPN on the platform. */
interface VpnController {
    val state: StateFlow<VpnState>
    val traffic: StateFlow<Traffic>

    /** Asks for the VPN permission if needed, then connects. */
    suspend fun connect(profile: Profile)

    fun disconnect()
}
