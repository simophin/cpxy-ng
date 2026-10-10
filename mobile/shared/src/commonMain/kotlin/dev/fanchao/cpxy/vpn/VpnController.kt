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
    /** ISO 3166-1 alpha-2 code of the country [host] is in, when known. */
    val countryCode: String?,
)

/** A connection the engine recorded, numbered by [seq]. */
data class ConnectionRecord(
    /** Increases with every connection, and is never reused while the app runs. */
    val seq: Long,
    val event: ConnectionEvent,
)

data class ConnectionPage(
    /** Oldest first. */
    val records: List<ConnectionRecord>,
    /**
     * The [ConnectionRecord.seq] of the oldest connection the engine still keeps, or of the next
     * one when it keeps none. Connections before it are gone.
     */
    val oldestSeq: Long,
)

/** Starts and stops the VPN on the platform. */
interface VpnController {
    val state: StateFlow<VpnState>
    val traffic: StateFlow<Traffic>

    /**
     * The oldest [limit] of the engine's recent connections from [since] on. When [since] is
     * before [ConnectionPage.oldestSeq], connections in between were dropped. Empty while
     * disconnected.
     */
    suspend fun connectionsSince(since: Long, limit: Int): ConnectionPage

    /**
     * The newest [limit] of the engine's recent connections before [before], or the newest of all
     * when it is null. Empty while disconnected.
     */
    suspend fun connectionsBefore(before: Long?, limit: Int): ConnectionPage

    /** Asks for the VPN permission if needed, then connects. */
    suspend fun connect(profile: Profile)

    fun disconnect()
}
