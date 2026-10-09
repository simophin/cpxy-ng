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
import dev.fanchao.cpxy.vpn.ui.CpxyVpnApp
import kotlinx.coroutines.CompletableDeferred

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
        setContent {
            CpxyVpnApp(repository = app.repository, controller = app.controller)
        }
    }

    override fun onDestroy() {
        if (app.controller.permissionRequester === requestConsent) {
            app.controller.permissionRequester = null
        }
        consentResult?.cancel()
        super.onDestroy()
    }
}
