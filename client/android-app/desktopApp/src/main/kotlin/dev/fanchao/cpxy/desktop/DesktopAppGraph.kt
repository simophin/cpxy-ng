package dev.fanchao.cpxy.desktop

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import dev.fanchao.cpxy.app.AppGraph
import dev.fanchao.cpxy.app.AppLogger
import dev.fanchao.cpxy.app.AppScope
import dev.fanchao.cpxy.app.CONFIG_DATASTORE_FILE_NAME
import dev.fanchao.cpxy.app.createPreferencesDataStore
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
import okio.Path.Companion.toOkioPath
import java.nio.file.Files
import java.nio.file.Path

data class AppPaths(val configDirectory: Path) {
    companion object {
        fun forSystem(
            osName: String = System.getProperty("os.name"),
            environment: Map<String, String> = System.getenv(),
            userHome: String = System.getProperty("user.home"),
        ): AppPaths {
            val directory = when {
                osName.startsWith("Windows", ignoreCase = true) ->
                    Path.of(requireNotNull(environment["APPDATA"]) { "APPDATA is not set" }, "Cpxy")
                osName.startsWith("Mac", ignoreCase = true) ->
                    Path.of(userHome, "Library", "Application Support", "Cpxy")
                else -> environment["XDG_CONFIG_HOME"]?.takeIf(String::isNotBlank)
                    ?.let { Path.of(it, "cpxy") }
                    ?: Path.of(userHome, ".config", "cpxy")
            }
            return AppPaths(directory)
        }
    }
}
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

    @Provides @SingleIn(AppScope::class)
    fun provideConfigDataStore(
        appPaths: AppPaths,
        applicationScope: CoroutineScope,
    ): DataStore<Preferences> {
        Files.createDirectories(appPaths.configDirectory)
        return createPreferencesDataStore(
            appPaths.configDirectory.resolve(CONFIG_DATASTORE_FILE_NAME).toOkioPath(),
            applicationScope,
        )
    }
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
