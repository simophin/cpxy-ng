package dev.fanchao.cpxy.app

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.serialization.json.Json

sealed interface ConfigLoadState {
    data object Loading : ConfigLoadState
    data class Loaded(val config: ClientConfig) : ConfigLoadState
    data class Error(val cause: Throwable) : ConfigLoadState
}

@Inject
@SingleIn(AppScope::class)
class ConfigRepository(
    private val dataStore: DataStore<Preferences>,
    json: Json,
    applicationScope: CoroutineScope,
) {
    private val codec = ConfigJsonCodec(json)

    val loadState: StateFlow<ConfigLoadState> = dataStore.data
        .map { preferences -> decode(preferences[ConfigJsonKey]) }
        .catch { emit(ConfigLoadState.Error(it)) }
        .stateIn(applicationScope, SharingStarted.Eagerly, ConfigLoadState.Loading)

    suspend fun saveProxySettings(httpPort: UShort, socksPort: UShort, dnsSever: String) {
        mutate { config ->
            config.copy(httpProxyPort = httpPort, socks5ProxyPort = socksPort, dnsServer = dnsSever)
        }
    }

    suspend fun saveProfile(profile: Profile) {
        mutate { config ->
            val index = config.profiles.indexOfFirst { it.id == profile.id }
            config.copy(profiles = if (index >= 0) config.profiles.toMutableList().apply {
                this[index] = profile
            } else config.profiles + profile)
        }
    }

    suspend fun deleteProfile(id: String) {
        mutate { config ->
            config.copy(
                profiles = config.profiles.filter { it.id != id },
                enabledProfileId = config.enabledProfileId.takeUnless { it == id },
            )
        }
    }

    suspend fun setProfileEnabled(id: String?) {
        mutate { it.copy(enabledProfileId = id) }
    }

    private suspend fun mutate(transform: (ClientConfig) -> ClientConfig) {
        dataStore.edit { preferences ->
            val current = preferences[ConfigJsonKey]
                ?.let { codec.decodeAndValidate(it).getOrThrow() }
                ?: DEFAULT_CLIENT_CONFIG
            preferences[ConfigJsonKey] = codec.encode(transform(current))
        }
    }

    private fun decode(raw: String?): ConfigLoadState = if (raw == null) {
        ConfigLoadState.Loaded(DEFAULT_CLIENT_CONFIG)
    } else {
        codec.decodeAndValidate(raw).fold(
            onSuccess = ConfigLoadState::Loaded,
            onFailure = ConfigLoadState::Error,
        )
    }
}
