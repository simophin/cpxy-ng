package dev.fanchao.cpxy

import android.content.SharedPreferences
import kotlinx.coroutines.GlobalScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.drop
import kotlinx.coroutines.launch
import kotlinx.serialization.json.Json
import androidx.core.content.edit
import kotlinx.coroutines.flow.update

class ClientConfigurationRepository(
    prefs: SharedPreferences,
    json: Json
) {
    private val mutableConfigurations = MutableStateFlow(prefs.readConfigurations(json))

    val configurations: StateFlow<List<ClientConfiguration>> get() = mutableConfigurations

    init {
        GlobalScope.launch {
            configurations.drop(1)
                .collectLatest {
                    prefs.storeConfigurations(json, it)
                }
        }
    }

    fun save(config: ClientConfiguration) {
        check(config.isValid) {
            "Invalid config"
        }

        mutableConfigurations.update { configs ->
            val index = configurations.value.indexOfFirst { it.id == config.id }
            if (index >= 0) {
                configs.toMutableList().apply {
                    this[index] = config
                }
            } else {
                configs + config
            }
        }
    }

    fun delete(id: String) {
        mutableConfigurations.update { configs ->
            configs.filterNot { it.id == id }
        }
    }

    fun setConfigEnabled(id: String, enabled: Boolean) {
        mutableConfigurations.update { configs ->
            val index = configs.indexOfFirst { it.id == id }
            if (index >= 0) {
                configs.toMutableList().apply {
                    this[index] = this[index].copy(enabled = enabled)
                }
            } else {
                configs
            }
        }
    }


    companion object {
        private const val PREF_KEY = "configs"

        private fun SharedPreferences.storeConfigurations(
            json: Json,
            configs: List<ClientConfiguration>
        ) {
            edit {
                putString(PREF_KEY, json.encodeToString(configs))
            }
        }

        private fun SharedPreferences.readConfigurations(
            json: Json
        ): List<ClientConfiguration> {
            return getString(PREF_KEY, null)?.let {
                json.decodeFromString(it)
            } ?: emptyList()
        }
    }
}