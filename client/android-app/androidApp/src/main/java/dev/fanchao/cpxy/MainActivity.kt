package dev.fanchao.cpxy

import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.remember
import androidx.core.content.ContextCompat
import dev.fanchao.cpxy.ui.CpxyApp
import dev.fanchao.cpxy.ui.events.AndroidEventTextFormatter
import dev.fanchao.cpxy.ui.theme.AndroidCpxyTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        if (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
        
        enableEdgeToEdge()
        val controller = appGraph.appController
        setContent {
            val eventTextFormatter = remember { AndroidEventTextFormatter() }

            AndroidCpxyTheme {
                CpxyApp(
                    controller = controller,
                    eventTextFormatter = eventTextFormatter,
                )
            }
        }
    }
}
