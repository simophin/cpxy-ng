package dev.fanchao.cpxy.vpn

import android.content.Context
import android.content.Intent
import android.net.VpnService
import androidx.core.content.ContextCompat
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map

/** Starts and stops [CpxyVpnService], which reports back through the `report` methods. */
class AndroidVpnController(private val context: Context) : VpnController {
    private val mutableState = MutableStateFlow<VpnState>(VpnState.Disconnected)
    private val mutableTraffic = MutableStateFlow(Traffic())
    private val mutableConnections = MutableSharedFlow<ConnectionEvent>(
        extraBufferCapacity = CONNECTION_BUFFER,
        onBufferOverflow = BufferOverflow.DROP_OLDEST,
    )

    override val state: StateFlow<VpnState> = mutableState.asStateFlow()
    override val traffic: StateFlow<Traffic> = mutableTraffic.asStateFlow()
    override val connections: SharedFlow<ConnectionEvent> = mutableConnections.asSharedFlow()

    /** Whether anything collects [connections], so the engine should report them. */
    internal val connectionsWanted: Flow<Boolean> = mutableConnections.subscriptionCount
        .map { it > 0 }
        .distinctUntilChanged()

    /**
     * Shows the system's VPN consent dialog and returns whether the user agreed. Set by the
     * activity while it exists.
     */
    var permissionRequester: (suspend (Intent) -> Boolean)? = null

    override suspend fun connect(profile: Profile) {
        VpnService.prepare(context)?.let { consent ->
            val requester = permissionRequester
            if (requester == null || !requester(consent)) {
                mutableState.value = VpnState.Failed("The VPN permission was not granted")
                return
            }
        }
        mutableState.value = VpnState.Connecting(profile.id)
        mutableTraffic.value = Traffic()
        ContextCompat.startForegroundService(context, CpxyVpnService.startIntent(context, profile))
    }

    override fun disconnect() {
        context.startService(CpxyVpnService.stopIntent(context))
    }

    internal fun reportState(state: VpnState) {
        mutableState.value = state
    }

    internal fun reportTraffic(traffic: Traffic) {
        mutableTraffic.value = traffic
    }

    /** Called from engine threads. */
    internal fun reportConnection(event: ConnectionEvent) {
        mutableConnections.tryEmit(event)
    }

    private companion object {
        /** Absorbs bursts while collectors catch up; the oldest are dropped beyond it. */
        const val CONNECTION_BUFFER = 1024
    }
}
