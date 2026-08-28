package dev.fanchao.cpxy

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import dev.fanchao.cpxy.app.AppGraph
import dev.fanchao.cpxy.app.AppScope
import dev.fanchao.cpxy.app.CONFIG_DATASTORE_FILE_NAME
import dev.fanchao.cpxy.app.ConfigJsonCodec
import dev.fanchao.cpxy.app.LegacyConfigMigration
import dev.fanchao.cpxy.app.createPreferencesDataStore
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
import okio.Path.Companion.toPath

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
    fun provideConfigDataStore(
        context: Context,
        applicationScope: CoroutineScope,
        json: Json,
    ): DataStore<Preferences> = createPreferencesDataStore(
        path = "${context.filesDir.absolutePath}/$CONFIG_DATASTORE_FILE_NAME".toPath(),
        applicationScope = applicationScope,
        migrations = listOf(
            LegacyConfigMigration(
                source = AndroidLegacyConfigSource(context),
                codec = ConfigJsonCodec(json),
            ),
        ),
    )

    @Provides
    @SingleIn(AppScope::class)
    fun provideHttpClient(): HttpClient = HttpClient(CIO) { install(WebSockets) }
}
