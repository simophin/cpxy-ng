package dev.fanchao.cpxy.vpn

import android.app.Application
import android.content.Context
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import okio.Path.Companion.toOkioPath

class App : Application() {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    val repository: ProfileRepository by lazy {
        ProfileRepository.create(filesDir.toOkioPath() / "profiles.preferences_pb", scope)
    }

    val settingsRepository: SettingsRepository by lazy {
        SettingsRepository.create(filesDir.toOkioPath() / "settings.preferences_pb", scope)
    }

    val controller: AndroidVpnController by lazy { AndroidVpnController(this) }
}

val Context.app: App get() = applicationContext as App
