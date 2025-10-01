package dev.fanchao.cpxy

import android.content.SharedPreferences
import androidx.core.content.edit
import kotlinx.coroutines.DelicateCoroutinesApi
import kotlinx.coroutines.GlobalScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json

@OptIn(DelicateCoroutinesApi::class)
class ConfigRepository(
    prefs: SharedPreferences,
    json: Json
) {
    private val mutableClientConfig = MutableStateFlow(
        prefs.getString(PREF_KEY, null)
            ?.let { json.decodeFromString(it) }
            ?: ClientConfig(
                profiles = emptyList(),
                enabledProfileId = null,
                httpProxyPort = 8080u,
                socks5ProxyPort = 1080u,
            )
    )

    val clientConfig: StateFlow<ClientConfig> get() = mutableClientConfig


    init {
        GlobalScope.launch {
            clientConfig.drop(1)
                .collectLatest {
                    prefs.edit {
                        putString(PREF_KEY, json.encodeToString(it))
                    }
                }
        }
    }

    fun saveProxySettings(httpPort: UShort, socksPort: UShort) {
        mutableClientConfig.update { config ->
            config.copy(
                httpProxyPort = httpPort,
                socks5ProxyPort = socksPort,
            )
        }
    }

    fun saveProfile(profile: Profile) {
        mutableClientConfig.update { config ->
            val index = config.profiles.indexOfFirst { it.id == profile.id }
            config.copy(
                profiles = if (index >= 0) {
                    config.profiles.toMutableList().apply {
                        this[index] = profile
                    }
                } else {
                    config.profiles + profile
                }
            )
        }
    }

    fun deleteProfile(id: String) {
        mutableClientConfig.update { config ->
            config.copy(
                profiles = config.profiles.filter { it.id != id },
                enabledProfileId = if (config.enabledProfileId == id) null else config.enabledProfileId
            )
        }
    }

    fun setProfileEnabled(id: String?) {
        mutableClientConfig.update { config ->
            config.copy(
                enabledProfileId = id
            )
        }
    }


    companion object {
        private const val PREF_KEY = "config"

    }
}