package dev.fanchao.cpxy.vpn

import android.annotation.SuppressLint
import android.app.PendingIntent
import android.content.Intent
import android.net.VpnService
import android.os.Build
import android.service.quicksettings.Tile
import android.service.quicksettings.TileService
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/** The quick settings tile: connects with the selected profile, or disconnects. */
class CpxyTileService : TileService() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
    private var listening: Job? = null

    override fun onStartListening() {
        listening?.cancel()
        listening = scope.launch {
            combine(app.controller.state, app.repository.profiles, ::Pair).collect { (state, stored) ->
                val tile = qsTile ?: return@collect
                fun nameOf(id: String) = stored.profiles.firstOrNull { it.id == id }?.name
                tile.state = when (state) {
                    is VpnState.Connecting, is VpnState.Connected -> Tile.STATE_ACTIVE
                    else -> Tile.STATE_INACTIVE
                }
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    tile.subtitle = when (state) {
                        is VpnState.Connecting -> "Connecting…"
                        is VpnState.Connected -> nameOf(state.profileId)
                        is VpnState.Failed -> "Failed"
                        VpnState.Disconnected -> stored.selected?.name ?: "No profile"
                    }
                }
                tile.updateTile()
            }
        }
    }

    override fun onStopListening() {
        listening?.cancel()
        listening = null
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    override fun onClick() {
        val controller = app.controller
        when (controller.state.value) {
            is VpnState.Connecting, is VpnState.Connected -> controller.disconnect()
            else -> scope.launch {
                val profile = app.repository.profiles.first().selected
                when {
                    profile == null -> unlockAndRun { openApp(Intent.ACTION_MAIN) }
                    // The consent dialog needs the activity.
                    VpnService.prepare(this@CpxyTileService) != null ->
                        unlockAndRun { openApp(MainActivity.ACTION_CONNECT) }
                    else -> controller.connect(profile)
                }
            }
        }
    }

    @SuppressLint("StartActivityAndCollapseDeprecated")
    private fun openApp(action: String) {
        val intent = Intent(this, MainActivity::class.java)
            .setAction(action)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startActivityAndCollapse(
                PendingIntent.getActivity(this, 0, intent, PendingIntent.FLAG_IMMUTABLE),
            )
        } else {
            @Suppress("DEPRECATION")
            startActivityAndCollapse(intent)
        }
    }
}
