package dev.fanchao.cpxy.vpn

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.map
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import okio.Path

/** Which apps the VPN carries. */
@Serializable
enum class AppFilter {
    AllApps,

    /** Only [AppSettings.onlyApps] use the VPN (an allowlist). */
    OnlySelected,

    /** [AppSettings.exceptApps] bypass the VPN (a blocklist). */
    ExceptSelected,
}

/** Settings that apply to every profile. Each list keeps its apps while the other filter is used. */
@Serializable
data class AppSettings(
    val appFilter: AppFilter = AppFilter.AllApps,
    /** Package names, for [AppFilter.OnlySelected]. */
    val onlyApps: Set<String> = emptySet(),
    /** Package names, for [AppFilter.ExceptSelected]. */
    val exceptApps: Set<String> = emptySet(),
) {
    /** The apps [filter] applies to. */
    fun appsFor(filter: AppFilter): Set<String> = when (filter) {
        AppFilter.AllApps -> emptySet()
        AppFilter.OnlySelected -> onlyApps
        AppFilter.ExceptSelected -> exceptApps
    }

    fun withApps(filter: AppFilter, apps: Set<String>): AppSettings = when (filter) {
        AppFilter.AllApps -> this
        AppFilter.OnlySelected -> copy(onlyApps = apps)
        AppFilter.ExceptSelected -> copy(exceptApps = apps)
    }

    /**
     * The apps the VPN carries, leaving out [ownPackage] so the engine's own sockets bypass it.
     * An allowlist with no apps carries every app, as the platforms do with an empty one.
     */
    fun routing(ownPackage: String): AppRouting {
        val only = onlyApps - ownPackage
        return if (appFilter == AppFilter.OnlySelected && only.isNotEmpty()) {
            AppRouting.Only(only)
        } else {
            AppRouting.Except(
                if (appFilter == AppFilter.ExceptSelected) exceptApps + ownPackage else setOf(ownPackage),
            )
        }
    }
}

sealed interface AppRouting {
    data class Only(val packages: Set<String>) : AppRouting
    data class Except(val packages: Set<String>) : AppRouting
}

/** Keeps the [AppSettings] as one JSON value in a preferences DataStore. */
class SettingsRepository(private val dataStore: DataStore<Preferences>) {
    val settings: Flow<AppSettings> = dataStore.data.map { decode(it[SettingsKey]) }

    suspend fun update(transform: (AppSettings) -> AppSettings) {
        dataStore.edit { preferences ->
            preferences[SettingsKey] = json.encodeToString(transform(decode(preferences[SettingsKey])))
        }
    }

    private fun decode(raw: String?): AppSettings =
        raw?.let { json.decodeFromString<AppSettings>(it) } ?: AppSettings()

    companion object {
        private val SettingsKey = stringPreferencesKey("settings_json")
        private val json = Json { ignoreUnknownKeys = true }

        fun create(path: Path, scope: CoroutineScope): SettingsRepository {
            require(path.name.endsWith(".preferences_pb")) {
                "DataStore path must end with .preferences_pb: $path"
            }
            return SettingsRepository(
                PreferenceDataStoreFactory.createWithPath(scope = scope, produceFile = { path })
            )
        }
    }
}
