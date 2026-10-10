package dev.fanchao.cpxy.vpn

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.content.ContextCompat
import androidx.lifecycle.lifecycleScope
import dev.fanchao.cpxy.vpn.ui.CpxyVpnApp
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

class MainActivity : ComponentActivity() {
    private var consentResult: CompletableDeferred<Boolean>? = null
    private val vpnConsent = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) {
        consentResult?.complete(it.resultCode == RESULT_OK)
        consentResult = null
    }
    private val notificationPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) {}

    private val requestConsent: suspend (Intent) -> Boolean = { intent ->
        consentResult?.cancel()
        val result = CompletableDeferred<Boolean>().also { consentResult = it }
        vpnConsent.launch(intent)
        result.await()
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        if (
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
        }

        app.controller.permissionRequester = requestConsent
        enableEdgeToEdge()
        val platform = AndroidPlatform(this)
        setContent {
            CpxyVpnApp(
                repository = app.repository,
                settingsRepository = app.settingsRepository,
                controller = app.controller,
                platform = platform,
            )
        }
        if (savedInstanceState == null) handle(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    /** Connects for the quick settings tile, which needs the activity for the VPN consent. */
    private fun handle(intent: Intent) {
        if (intent.action != ACTION_CONNECT) return
        lifecycleScope.launch {
            app.repository.profiles.first().selected?.let { app.controller.connect(it) }
        }
    }

    override fun onDestroy() {
        if (app.controller.permissionRequester === requestConsent) {
            app.controller.permissionRequester = null
        }
        consentResult?.cancel()
        super.onDestroy()
    }

    companion object {
        const val ACTION_CONNECT = "dev.fanchao.cpxy.vpn.CONNECT"
    }
}
