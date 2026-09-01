package dev.fanchao.cpxy.app

import androidx.datastore.preferences.core.mutablePreferencesOf
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertFalse
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json

class ConfigPersistencePrimitivesTest {
    private val codec = ConfigJsonCodec(Json { ignoreUnknownKeys = true; isLenient = true })

    @Test
    fun defaultConfigurationUsesStableValues() {
        assertTrue(DEFAULT_CLIENT_CONFIG.profiles.isEmpty())
        assertNull(DEFAULT_CLIENT_CONFIG.enabledProfileId)
        assertEquals(8_080u.toUShort(), DEFAULT_CLIENT_CONFIG.httpProxyPort)
        assertEquals(1_080u.toUShort(), DEFAULT_CLIENT_CONFIG.socks5ProxyPort)
        assertEquals(3_010u.toUShort(), DEFAULT_CLIENT_CONFIG.apiServerPort)
        assertEquals("223.5.5.5", DEFAULT_CLIENT_CONFIG.dnsServer)
        assertTrue(CONFIG_DATASTORE_FILE_NAME.endsWith(".preferences_pb"))
    }

    @Test
    fun codecRoundTripsAValidConfiguration() {
        val config = validConfig()

        assertEquals(config, codec.decodeAndValidate(codec.encode(config)).getOrThrow())
    }

    @Test
    fun codecAcceptsLegacyDefaultsAndUnknownFields() {
        val raw = """
            {
              "profiles": [],
              "enabledProfileId": null,
              "httpProxyPort": 8080,
              "socks5ProxyPort": 1080,
              "futureSetting": "kept in stored JSON"
            }
        """.trimIndent()

        val decoded = codec.decodeAndValidate(raw).getOrThrow()

        assertEquals(3_010u.toUShort(), decoded.apiServerPort)
        assertEquals("223.5.5.5", decoded.dnsServer)
    }

    @Test
    fun codecRejectsMalformedAndInvalidConfiguration() {
        assertTrue(codec.decodeAndValidate("not json").isFailure)
        assertTrue(codec.decodeAndValidate(codecJson(validConfig(httpProxyPort = 0u))).isFailure)
        assertFailsWith<IllegalArgumentException> { codec.encode(validConfig(dnsServer = " ")) }
    }

    @Test
    fun validationReportsPortsProfilesAndEnabledProfileProblems() {
        val duplicate = validProfile(id = "duplicate")
        val errors = ClientConfig(
            profiles = listOf(
                duplicate,
                duplicate.copy(name = " ", mainServerUrl = ""),
            ),
            enabledProfileId = "missing",
            httpProxyPort = 0u,
            socks5ProxyPort = 3_010u,
            apiServerPort = 3_010u,
            dnsServer = "",
        ).validationErrors()

        val fields = errors.map { it.field }.toSet()
        assertTrue("httpProxyPort" in fields)
        assertTrue("socks5ProxyPort" in fields)
        assertTrue("apiServerPort" in fields)
        assertTrue("dnsServer" in fields)
        assertTrue("profiles" in fields)
        assertTrue("profiles[1].name" in fields)
        assertTrue("profiles[1].mainServerUrl" in fields)
        assertTrue("enabledProfileId" in fields)
    }

    @Test
    fun migrationCopiesValidLegacyJsonAndAlwaysFlagsCompletion() = runTest {
        val raw = codecJson(validConfig())
        val migration = LegacyConfigMigration(LegacyConfigSource { raw }, codec)

        val migrated = migration.migrate(mutablePreferencesOf())

        assertEquals(raw, migrated[ConfigJsonKey])
        assertEquals(true, migrated[LegacyMigrationCompleteKey])
        assertFalse(migration.shouldMigrate(migrated))
    }

    @Test
    fun migrationMarksMissingOrMalformedLegacyDataWithoutCopyingIt() = runTest {
        listOf<String?>(null, "not json", codecJson(validConfig(httpProxyPort = 0u))).forEach { raw ->
            val migration = LegacyConfigMigration(LegacyConfigSource { raw }, codec)

            val migrated = migration.migrate(mutablePreferencesOf())

            assertNull(migrated[ConfigJsonKey])
            assertEquals(true, migrated[LegacyMigrationCompleteKey])
        }
    }

    @Test
    fun migrationNeverOverwritesDataStoreOrReadsLegacySourceWhenPopulated() = runTest {
        var reads = 0
        val existing = codecJson(validConfig(dnsServer = "1.1.1.1"))
        val migration = LegacyConfigMigration(
            source = LegacyConfigSource {
                reads++
                codecJson(validConfig(dnsServer = "8.8.8.8"))
            },
            codec = codec,
        )

        val migrated = migration.migrate(mutablePreferencesOf(ConfigJsonKey to existing))

        assertEquals(existing, migrated[ConfigJsonKey])
        assertEquals(true, migrated[LegacyMigrationCompleteKey])
        assertEquals(0, reads)
    }

    @Test
    fun completedMigrationDoesNotRunAgain() = runTest {
        val migration = LegacyConfigMigration(LegacyConfigSource { error("must not read") }, codec)
        val preferences = mutablePreferencesOf(LegacyMigrationCompleteKey to true)

        assertFalse(migration.shouldMigrate(preferences))
        migration.cleanUp()
    }

    private fun validConfig(
        httpProxyPort: UShort = 8_080u,
        dnsServer: String = "223.5.5.5",
    ) = ClientConfig(
        profiles = listOf(validProfile()),
        enabledProfileId = "primary",
        httpProxyPort = httpProxyPort,
        socks5ProxyPort = 1_080u,
        dnsServer = dnsServer,
    )

    private fun validProfile(id: String = "primary") = Profile(
        id = id,
        name = "Primary",
        mainServerUrl = "wss://main.example.test",
        aiServerUrl = null,
        tailscaleServerUrl = null,
    )

    private fun codecJson(config: ClientConfig): String =
        Json.encodeToString(ClientConfig.serializer(), config)
}
