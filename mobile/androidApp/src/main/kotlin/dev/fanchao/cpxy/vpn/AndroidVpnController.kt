package dev.fanchao.cpxy.vpn

import android.content.Context
import android.content.Intent
import android.net.VpnService
import androidx.core.content.ContextCompat
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update

/** Starts and stops [CpxyVpnService], which reports back through the `report` methods. */
class AndroidVpnController(private val context: Context) : VpnController {
    private val mutableState = MutableStateFlow<VpnState>(VpnState.Disconnected)
    private val mutableTraffic = MutableStateFlow(Traffic())
    private val mutableConnections = MutableStateFlow<List<ConnectionEvent>>(emptyList())

    override val state: StateFlow<VpnState> = mutableState.asStateFlow()
    override val traffic: StateFlow<Traffic> = mutableTraffic.asStateFlow()
    override val connections: StateFlow<List<ConnectionEvent>> = mutableConnections.asStateFlow()

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
        mutableConnections.value = emptyList()
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

    internal fun reportConnection(event: ConnectionEvent) {
        mutableConnections.update { (listOf(event) + it).take(MAX_CONNECTIONS) }
    }

    private companion object {
        const val MAX_CONNECTIONS = 100
    }
}
