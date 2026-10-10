package dev.fanchao.cpxy.vpn

import android.content.Context
import android.content.Intent
import android.net.VpnService
import androidx.core.content.ContextCompat
import dev.fanchao.cpxy.vpn.engine.Engine
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.withContext
import dev.fanchao.cpxy.vpn.engine.ConnectionPage as EngineConnectionPage

/** Starts and stops [CpxyVpnService], which reports back through the `report` methods. */
class AndroidVpnController(private val context: Context) : VpnController {
    private val mutableState = MutableStateFlow<VpnState>(VpnState.Disconnected)
    private val mutableTraffic = MutableStateFlow(Traffic())

    /** The running engine, set by [CpxyVpnService]. */
    @Volatile
    internal var engine: Engine? = null

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

    override suspend fun connectionsSince(since: Long, limit: Int): ConnectionPage =
        queryConnections { it.connectionsSince(since.toULong(), limit.toUInt()) }

    override suspend fun connectionsBefore(before: Long?, limit: Int): ConnectionPage =
        queryConnections { it.connectionsBefore(before?.toULong(), limit.toUInt()) }

    private suspend fun queryConnections(query: (Engine) -> EngineConnectionPage): ConnectionPage =
        withContext(Dispatchers.Default) {
            val page = engine?.let {
                // The service closes the engine when it stops, maybe while this runs.
                try {
                    query(it)
                } catch (_: IllegalStateException) {
                    null
                }
            } ?: return@withContext ConnectionPage(emptyList(), oldestSeq = 0)
            ConnectionPage(
                records = page.records.map { record ->
                    val event = record.event
                    ConnectionRecord(
                        seq = record.seq.toLong(),
                        event = ConnectionEvent(
                            host = event.host,
                            port = event.port.toInt(),
                            outbound = event.outbound,
                            delayMillis = event.delayMillis.toLong(),
                            timeMillis = event.timeMillis.toLong(),
                            error = event.error,
                            countryCode = event.countryCode,
                        ),
                    )
                },
                oldestSeq = page.oldestSeq.toLong(),
            )
        }
}
