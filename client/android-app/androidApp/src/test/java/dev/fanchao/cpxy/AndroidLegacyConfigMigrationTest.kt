package dev.fanchao.cpxy

import android.content.Context
import androidx.datastore.preferences.core.mutablePreferencesOf
import dev.fanchao.cpxy.app.ClientConfig
import dev.fanchao.cpxy.app.ConfigJsonCodec
import dev.fanchao.cpxy.app.ConfigJsonKey
import dev.fanchao.cpxy.app.LegacyConfigMigration
import dev.fanchao.cpxy.app.LegacyMigrationCompleteKey
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment

@RunWith(RobolectricTestRunner::class)
class AndroidLegacyConfigMigrationTest {
    private lateinit var context: Context
    private val codec = ConfigJsonCodec(Json { ignoreUnknownKeys = true; isLenient = true })

    @Before
    fun clearLegacyPreferences() {
        context = RuntimeEnvironment.getApplication()
        context.getSharedPreferences(LEGACY_PREFERENCES_NAME, Context.MODE_PRIVATE)
            .edit()
            .clear()
            .commit()
    }

    @Test
    fun validLegacyConfigIsCopiedAsRawJsonAndMarkedComplete() = runTest {
        val raw = validConfigJson()
        putLegacy(raw)

        val migrated = migration().migrate(mutablePreferencesOf())

        assertEquals(raw, migrated[ConfigJsonKey])
        assertEquals(true, migrated[LegacyMigrationCompleteKey])
    }

    @Test
    fun missingLegacyConfigMarksCompleteAndLeavesConfigAbsent() = runTest {
        val migrated = migration().migrate(mutablePreferencesOf())

        assertNull(migrated[ConfigJsonKey])
        assertEquals(true, migrated[LegacyMigrationCompleteKey])
    }

    @Test
    fun malformedLegacyConfigMarksCompleteAndLeavesConfigAbsent() = runTest {
        putLegacy("not json")

        val migrated = migration().migrate(mutablePreferencesOf())

        assertNull(migrated[ConfigJsonKey])
        assertEquals(true, migrated[LegacyMigrationCompleteKey])
    }

    @Test
    fun alreadyFlaggedMigrationDoesNothing() = runTest {
        val legacy = validConfigJson(httpProxyPort = 9_090u)
        val existing = validConfigJson(httpProxyPort = 8_181u)
        putLegacy(legacy)
        val current = mutablePreferencesOf(
            LegacyMigrationCompleteKey to true,
            ConfigJsonKey to existing,
        )
        val migration = migration()

        assertFalse(migration.shouldMigrate(current))
        assertEquals(existing, current[ConfigJsonKey])
        assertEquals(legacy, legacyPreferences().getString(LEGACY_CONFIG_KEY, null))
    }

    @Test
    fun dataStoreConfigIsNeverOverwritten() = runTest {
        putLegacy(validConfigJson(httpProxyPort = 9_090u))
        val existing = validConfigJson(httpProxyPort = 8_181u)

        val migrated = migration().migrate(mutablePreferencesOf(ConfigJsonKey to existing))

        assertEquals(existing, migrated[ConfigJsonKey])
        assertEquals(true, migrated[LegacyMigrationCompleteKey])
    }

    @Test
    fun migrationRetainsLegacySharedPreferencesForRollback() = runTest {
        val raw = validConfigJson()
        putLegacy(raw)
        val migration = migration()

        migration.migrate(mutablePreferencesOf())
        migration.cleanUp()

        assertEquals(raw, legacyPreferences().getString(LEGACY_CONFIG_KEY, null))
        assertTrue(legacyPreferences().contains(LEGACY_CONFIG_KEY))
    }

    private fun migration() = LegacyConfigMigration(AndroidLegacyConfigSource(context), codec)

    private fun putLegacy(raw: String) {
        check(legacyPreferences().edit().putString(LEGACY_CONFIG_KEY, raw).commit())
    }

    private fun legacyPreferences() =
        context.getSharedPreferences(LEGACY_PREFERENCES_NAME, Context.MODE_PRIVATE)

    private fun validConfigJson(httpProxyPort: UShort = 8_080u): String = Json.encodeToString(
        ClientConfig.serializer(),
        ClientConfig(
            profiles = emptyList(),
            enabledProfileId = null,
            httpProxyPort = httpProxyPort,
            socks5ProxyPort = 1_080u,
        ),
    )
}
