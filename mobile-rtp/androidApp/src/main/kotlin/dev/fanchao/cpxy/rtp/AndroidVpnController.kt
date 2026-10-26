package dev.fanchao.cpxy.rtp

import android.content.Context
import android.content.Intent
import android.net.VpnService
import androidx.core.content.ContextCompat
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/** Starts and stops [RtpVpnService], which reports back through the `report` methods. */
class AndroidVpnController(private val context: Context) : VpnController {
    private val mutableState = MutableStateFlow<VpnState>(VpnState.Disconnected)
    private val mutableTraffic = MutableStateFlow(Traffic())

    override val state: StateFlow<VpnState> = mutableState.asStateFlow()
    override val traffic: StateFlow<Traffic> = mutableTraffic.asStateFlow()

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
        mutableState.value = VpnState.Starting(profile.id)
        mutableTraffic.value = Traffic()
        ContextCompat.startForegroundService(context, RtpVpnService.startIntent(context, profile.id))
    }

    override fun disconnect() {
        context.startService(RtpVpnService.stopIntent(context))
    }

    internal fun reportState(state: VpnState) {
        mutableState.value = state
    }

    internal fun reportTraffic(traffic: Traffic) {
        mutableTraffic.value = traffic
    }
}
