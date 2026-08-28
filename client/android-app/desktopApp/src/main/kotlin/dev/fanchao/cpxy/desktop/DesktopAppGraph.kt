package dev.fanchao.cpxy.desktop

import dev.fanchao.cpxy.app.AppGraph
import dev.fanchao.cpxy.app.AppLogger
import dev.fanchao.cpxy.app.AppScope
import dev.fanchao.cpxy.app.ConfigPersistence
import dev.fanchao.cpxy.app.NativeClient
import dev.fanchao.cpxy.app.NativeClientConfig
import dev.fanchao.cpxy.app.NativeClientSession
import dev.zacsweers.metro.ContributesBinding
import dev.zacsweers.metro.DependencyGraph
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.Provides
import dev.zacsweers.metro.SingleIn
import io.ktor.client.HttpClient
import io.ktor.client.engine.cio.CIO
import io.ktor.client.plugins.websocket.WebSockets
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.serialization.json.Json

data class AppPaths(val dataDirectory: String)
data class NativeLibraryPath(val value: String)

@DependencyGraph(AppScope::class)
interface DesktopAppGraph : AppGraph {
    @DependencyGraph.Factory
    fun interface Factory {
        fun create(
            @Provides appPaths: AppPaths,
            @Provides nativeLibraryPath: NativeLibraryPath,
        ): DesktopAppGraph
    }

    @Provides @SingleIn(AppScope::class)
    fun provideApplicationScope(): CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    @Provides @SingleIn(AppScope::class)
    fun provideJson(): Json = Json { ignoreUnknownKeys = true; isLenient = true }

    @Provides @SingleIn(AppScope::class)
    fun provideHttpClient(): HttpClient = HttpClient(CIO) { install(WebSockets) }
}

@Inject
@ContributesBinding(AppScope::class)
@SingleIn(AppScope::class)
class DesktopConfigPersistence(appPaths: AppPaths) : ConfigPersistence {
    // DataStore-backed persistence is introduced in Phase 4. Retain a process-local bridge until then.
    private var value: String? = null
    init { appPaths.dataDirectory.length }
    override fun load(): String? = value
    override fun save(value: String) { this.value = value }
}

@Inject
@ContributesBinding(AppScope::class)
@SingleIn(AppScope::class)
class DesktopNativeClient(private val nativeLibraryPath: NativeLibraryPath) : NativeClient {
    override fun start(config: NativeClientConfig): NativeClientSession {
        error("Desktop native loading is introduced in the native integration phase: ${nativeLibraryPath.value}")
    }
}

@Inject
@ContributesBinding(AppScope::class)
@SingleIn(AppScope::class)
class DesktopAppLogger : AppLogger {
    override fun debug(tag: String, message: String) = println("D/$tag: $message")
    override fun error(tag: String, message: String, throwable: Throwable?) {
        System.err.println("E/$tag: $message")
        throwable?.printStackTrace()
    }
}
