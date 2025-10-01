package dev.fanchao.cpxy

import android.app.Application
import android.content.Context
import com.sun.jna.Native
import kotlinx.serialization.json.Json

class App : Application() {
    val client: Client by lazy {
        Native.load("client", Client::class.java) as Client
    }

    val configurationRepository: ConfigRepository by lazy {
        ConfigRepository(
            prefs = getSharedPreferences("default", MODE_PRIVATE),
            json = Json {
                ignoreUnknownKeys = true
                isLenient = true
            }
        )
    }

    val profileInstanceManager: ProfileInstanceManager by lazy {
        ProfileInstanceManager(
            repository = configurationRepository,
            clientProvider = { client },
        )
    }

    override fun onCreate() {
        super.onCreate()

        ClientServiceCoordinator(
            appContext = this,
            profileInstanceManager = profileInstanceManager,
        )
    }

    companion object {
        val Context.appInstance: App
            get() = (applicationContext as App)
    }
}