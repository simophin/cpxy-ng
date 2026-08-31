package dev.fanchao.cpxy.app

import androidx.datastore.preferences.core.edit
import java.nio.file.Files
import kotlin.io.path.deleteIfExists
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import okio.Path.Companion.toOkioPath

class PreferencesDataStoreFactoryTest {
    @Test
    fun factoryPersistsPreferencesToARealTemporaryFile() = runTest {
        val directory = Files.createTempDirectory("cpxy-datastore-test")
        val file = directory.resolve(CONFIG_DATASTORE_FILE_NAME)
        val scope = CoroutineScope(coroutineContext + SupervisorJob())
        try {
            val dataStore = createPreferencesDataStore(file.toOkioPath(), scope)

            dataStore.edit { it[ConfigJsonKey] = "stored" }

            assertEquals("stored", dataStore.data.first()[ConfigJsonKey])
            assertEquals(true, Files.exists(file))
        } finally {
            scope.cancel()
            file.deleteIfExists()
            directory.deleteIfExists()
        }
    }

    @Test
    fun factoryRejectsAnInvalidFileSuffix() = runTest {
        val file = Files.createTempFile("cpxy-datastore-test", ".bin")
        try {
            assertFailsWith<IllegalArgumentException> {
                createPreferencesDataStore(file.toOkioPath(), this)
            }
        } finally {
            file.deleteIfExists()
        }
    }

    @Test
    fun factoryRunsLegacyMigrationAgainstRealStorage() = runTest {
        val directory = Files.createTempDirectory("cpxy-datastore-migration-test")
        val file = directory.resolve(CONFIG_DATASTORE_FILE_NAME)
        val scope = CoroutineScope(coroutineContext + SupervisorJob())
        val raw = Json.encodeToString(ClientConfig.serializer(), DEFAULT_CLIENT_CONFIG)
        try {
            val dataStore = createPreferencesDataStore(
                path = file.toOkioPath(),
                applicationScope = scope,
                migrations = listOf(
                    LegacyConfigMigration(
                        source = LegacyConfigSource { raw },
                        codec = ConfigJsonCodec(Json { ignoreUnknownKeys = true; isLenient = true }),
                    ),
                ),
            )

            val preferences = dataStore.data.first()

            assertEquals(raw, preferences[ConfigJsonKey])
            assertEquals(true, preferences[LegacyMigrationCompleteKey])
        } finally {
            scope.cancel()
            file.deleteIfExists()
            directory.deleteIfExists()
        }
    }
}
