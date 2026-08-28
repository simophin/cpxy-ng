package dev.fanchao.cpxy.app

import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json

@Inject
@SingleIn(AppScope::class)
class ConfigRepository(
    persistence: ConfigPersistence,
    private val json: Json,
    applicationScope: CoroutineScope,
) {
    private val mutableClientConfig = MutableStateFlow(
        persistence.load()?.let(json::decodeFromString) ?: DEFAULT_CONFIG,
    )

    val clientConfig: StateFlow<ClientConfig> get() = mutableClientConfig

    init {
        applicationScope.launch {
            clientConfig.drop(1).collectLatest { persistence.save(json.encodeToString(it)) }
        }
    }

    fun saveProxySettings(httpPort: UShort, socksPort: UShort, dnsSever: String) {
        mutableClientConfig.update {
            it.copy(httpProxyPort = httpPort, socks5ProxyPort = socksPort, dnsServer = dnsSever)
        }
    }

    fun saveProfile(profile: Profile) {
        mutableClientConfig.update { config ->
            val index = config.profiles.indexOfFirst { it.id == profile.id }
            config.copy(profiles = if (index >= 0) config.profiles.toMutableList().apply {
                this[index] = profile
            } else config.profiles + profile)
        }
    }

    fun deleteProfile(id: String) {
        mutableClientConfig.update { config ->
            config.copy(
                profiles = config.profiles.filter { it.id != id },
                enabledProfileId = config.enabledProfileId.takeUnless { it == id },
            )
        }
    }

    fun setProfileEnabled(id: String?) {
        mutableClientConfig.update { it.copy(enabledProfileId = id) }
    }

    private companion object {
        val DEFAULT_CONFIG = ClientConfig(
            profiles = emptyList(),
            enabledProfileId = null,
            httpProxyPort = 8080u,
            socks5ProxyPort = 1080u,
        )
    }
}
