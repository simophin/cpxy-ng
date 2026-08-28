package dev.fanchao.cpxy

import android.content.Context
import dev.fanchao.cpxy.app.AppGraph
import dev.fanchao.cpxy.app.AppScope
import dev.zacsweers.metro.DependencyGraph
import dev.zacsweers.metro.Provides
import dev.zacsweers.metro.SingleIn
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.websocket.WebSockets
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.serialization.json.Json

@DependencyGraph(AppScope::class)
interface AndroidAppGraph : AppGraph {
    val clientServiceCoordinator: ClientServiceCoordinator

    @DependencyGraph.Factory
    fun interface Factory {
        fun create(@Provides context: Context): AndroidAppGraph
    }

    @Provides
    @SingleIn(AppScope::class)
    fun provideApplicationScope(): CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    @Provides
    @SingleIn(AppScope::class)
    fun provideJson(): Json = Json { ignoreUnknownKeys = true; isLenient = true }

    @Provides
    @SingleIn(AppScope::class)
    fun provideHttpClient(): HttpClient = HttpClient(CIO) { install(WebSockets) }
}
