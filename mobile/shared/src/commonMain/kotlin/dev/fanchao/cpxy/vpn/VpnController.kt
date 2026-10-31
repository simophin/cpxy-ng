package dev.fanchao.cpxy.vpn

import kotlinx.coroutines.flow.StateFlow

sealed interface VpnState {
    data object Disconnected : VpnState
    data class Connecting(val profileId: String) : VpnState
    data class Connected(val profileId: String) : VpnState
    data class Failed(val message: String) : VpnState
}

/** Starts and stops the VPN on the platform. */
interface VpnController {
    val state: StateFlow<VpnState>
    /** Asks for the VPN permission if needed, then connects. */
    suspend fun connect(profile: Profile)

    fun disconnect()
}
