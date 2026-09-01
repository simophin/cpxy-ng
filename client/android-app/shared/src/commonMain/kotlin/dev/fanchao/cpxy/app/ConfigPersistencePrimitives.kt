package dev.fanchao.cpxy.app

import androidx.datastore.core.DataMigration
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.CoroutineScope
import kotlinx.serialization.json.Json
import okio.Path

const val CONFIG_DATASTORE_FILE_NAME = "cpxy.preferences_pb"

val ConfigJsonKey = stringPreferencesKey("client_config_json")
val LegacyMigrationCompleteKey = booleanPreferencesKey("legacy_config_migrated")

val DEFAULT_CLIENT_CONFIG = ClientConfig(
    profiles = emptyList(),
    enabledProfileId = null,
    httpProxyPort = 8_080u,
    socks5ProxyPort = 1_080u,
)

fun createPreferencesDataStore(
    path: Path,
    applicationScope: CoroutineScope,
    migrations: List<DataMigration<Preferences>> = emptyList(),
): DataStore<Preferences> {
    require(path.name.endsWith(".preferences_pb")) {
        "DataStore path must end with .preferences_pb: $path"
    }
    return PreferenceDataStoreFactory.createWithPath(
        migrations = migrations,
        scope = applicationScope,
        produceFile = { path },
    )
}

data class ConfigValidationError(
    val field: String,
    val message: String,
)

fun ClientConfig.validationErrors(): List<ConfigValidationError> = buildList {
    val ports = listOf(
        "httpProxyPort" to httpProxyPort,
        "socks5ProxyPort" to socks5ProxyPort,
        "apiServerPort" to apiServerPort,
    )
    ports.filter { (_, port) -> port == 0.toUShort() }.forEach { (field, _) ->
        add(ConfigValidationError(field, "Port must be greater than zero"))
    }
    ports.groupBy { (_, port) -> port }.values.filter { it.size > 1 }.flatten().forEach { (field, _) ->
        add(ConfigValidationError(field, "Ports must be distinct"))
    }
    if (dnsServer.isBlank()) {
        add(ConfigValidationError("dnsServer", "DNS server must not be blank"))
    }

    profiles.forEachIndexed { index, profile ->
        if (profile.id.isBlank()) {
            add(ConfigValidationError("profiles[$index].id", "Profile id must not be blank"))
        }
        if (profile.name.isBlank()) {
            add(ConfigValidationError("profiles[$index].name", "Profile name must not be blank"))
        }
        if (profile.mainServerUrl.isBlank()) {
            add(ConfigValidationError("profiles[$index].mainServerUrl", "Main server URL must not be blank"))
        }
    }

    profiles.groupBy(Profile::id).filterValues { it.size > 1 }.keys.forEach { duplicateId ->
        add(ConfigValidationError("profiles", "Profile id must be unique: $duplicateId"))
    }
    if (enabledProfileId != null && profiles.none { it.id == enabledProfileId }) {
        add(ConfigValidationError("enabledProfileId", "Enabled profile must exist"))
    }
}

class ConfigJsonCodec(
    private val json: Json,
) {
    fun encode(config: ClientConfig): String {
        requireValid(config)
        return json.encodeToString(config)
    }

    fun decodeAndValidate(raw: String): Result<ClientConfig> = runCatching {
        json.decodeFromString<ClientConfig>(raw).also(::requireValid)
    }

    private fun requireValid(config: ClientConfig) {
        val errors = config.validationErrors()
        require(errors.isEmpty()) {
            errors.joinToString(prefix = "Invalid client configuration: ") { "${it.field}: ${it.message}" }
        }
    }
}

fun interface LegacyConfigSource {
    suspend fun read(): String?
}

class LegacyConfigMigration(
    private val source: LegacyConfigSource,
    private val codec: ConfigJsonCodec,
) : DataMigration<Preferences> {
    override suspend fun shouldMigrate(currentData: Preferences): Boolean =
        currentData[LegacyMigrationCompleteKey] != true

    override suspend fun migrate(currentData: Preferences): Preferences {
        val existingConfig = currentData[ConfigJsonKey]
        val legacyConfig = if (existingConfig == null) source.read() else null
        val validLegacyConfig = legacyConfig?.takeIf { codec.decodeAndValidate(it).isSuccess }

        return currentData.toMutablePreferences().apply {
            if (existingConfig == null && validLegacyConfig != null) {
                this[ConfigJsonKey] = validLegacyConfig
            }
            this[LegacyMigrationCompleteKey] = true
        }.toPreferences()
    }

    override suspend fun cleanUp() = Unit
}
