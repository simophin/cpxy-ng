package dev.fanchao.cpxy

import android.app.Application
import android.content.Context
import com.sun.jna.Native
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.websocket.WebSockets
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

    val httpClient: HttpClient by lazy {
        HttpClient(CIO) {
            install(WebSockets)
        }
    }

    val json: Json by lazy {
        Json {
            ignoreUnknownKeys = true
            isLenient = true
        }
    }

    val profileInstanceManager: ProfileInstanceManager by lazy {
        ProfileInstanceManager(
            repository = configurationRepository,
            clientProvider = { client },
        )
    }

    val eventsRepository: EventsRepository by lazy {
        EventsRepository(
            manager = profileInstanceManager,
            client = httpClient,
            json = json,
        )
    }

    override fun onCreate() {
        super.onCreate()

        ClientServiceCoordinator(
            appContext = this,
            profileInstanceManager = profileInstanceManager,
        )

        // Initialize lazy properties
        eventsRepository.events
    }

    companion object {
        val Context.appInstance: App
            get() = (applicationContext as App)
    }
}