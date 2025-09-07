package dev.fanchao.cpxy

import android.app.Application
import android.content.Context
import com.sun.jna.Native
import kotlinx.serialization.json.Json

class App : Application() {
    val client: Client by lazy {
        Native.load("client", Client::class.java) as Client
    }

    val configurationRepository: ClientConfigurationRepository by lazy {
        ClientConfigurationRepository(
            prefs = getSharedPreferences("default", MODE_PRIVATE),
            json = Json {
                ignoreUnknownKeys = true
                isLenient = true
            }
        )
    }

    val clientInstanceManager: ClientInstanceManager by lazy {
        ClientInstanceManager(
            repository = configurationRepository,
            clientProvider = { client },
        )
    }

    companion object {
        val Context.appInstance: App
            get() = (applicationContext as App)
    }
}