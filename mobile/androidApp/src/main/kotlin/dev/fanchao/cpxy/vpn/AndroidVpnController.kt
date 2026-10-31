package dev.fanchao.cpxy.vpn

import android.content.Context
import android.content.Intent
import android.net.VpnService
import androidx.core.content.ContextCompat
import dev.fanchao.cpxy.vpn.engine.Engine
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import java.net.InetSocketAddress
import java.net.Proxy

/** Starts and stops [CpxyVpnService], which reports back through the `report` methods. */
class AndroidVpnController(private val context: Context) : VpnController {
    private val mutableState = MutableStateFlow<VpnState>(VpnState.Disconnected)

    /** The running engine, set by [CpxyVpnService]. */
    @Volatile
    internal var engine: Engine? = null

    override val state: StateFlow<VpnState> = mutableState.asStateFlow()

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
        ContextCompat.startForegroundService(context, CpxyVpnService.startIntent(context, profile))
    }

    override fun disconnect() {
        context.startService(CpxyVpnService.stopIntent(context))
    }

    internal fun reportState(state: VpnState) {
        mutableState.value = state
    }

    /**
     * The engine's proxy for this app's own requests, which bypass the VPN, or null while it is
     * not running.
     */
    fun localProxy(): Proxy? {
        val port = try {
            engine?.localProxyPort()
        } catch (_: IllegalStateException) {
            // The service closed the engine
            null
        } ?: return null
        // The engine listens on IPv4 only; the loopback address on Android is ::1
        return Proxy(Proxy.Type.HTTP, InetSocketAddress("127.0.0.1", port.toInt()))
    }
}
