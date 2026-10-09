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

@Serializable
data class StoredProfiles(
    val profiles: List<Profile> = emptyList(),
    /** The profile the connect button uses. */
    val selectedId: String? = null,
) {
    val selected: Profile? get() = profiles.firstOrNull { it.id == selectedId }
}

/** Keeps the profiles as one JSON value in a preferences DataStore. */
class ProfileRepository(private val dataStore: DataStore<Preferences>) {
    val profiles: Flow<StoredProfiles> = dataStore.data.map { decode(it[ProfilesKey]) }

    /** Adds the profile, or replaces the one with its ID, and selects it. */
    suspend fun save(profile: Profile) = mutate { stored ->
        val index = stored.profiles.indexOfFirst { it.id == profile.id }
        val profiles = if (index >= 0) {
            stored.profiles.toMutableList().apply { this[index] = profile }
        } else {
            stored.profiles + profile
        }
        StoredProfiles(profiles, selectedId = profile.id)
    }

    suspend fun delete(id: String) = mutate { stored ->
        val profiles = stored.profiles.filter { it.id != id }
        StoredProfiles(
            profiles,
            selectedId = stored.selectedId.takeUnless { it == id } ?: profiles.firstOrNull()?.id,
        )
    }

    suspend fun select(id: String) = mutate { it.copy(selectedId = id) }

    private suspend fun mutate(transform: (StoredProfiles) -> StoredProfiles) {
        dataStore.edit { preferences ->
            preferences[ProfilesKey] = json.encodeToString(transform(decode(preferences[ProfilesKey])))
        }
    }

    private fun decode(raw: String?): StoredProfiles =
        raw?.let { json.decodeFromString<StoredProfiles>(it) } ?: StoredProfiles()

    companion object {
        private val ProfilesKey = stringPreferencesKey("profiles_json")
        private val json = Json { ignoreUnknownKeys = true }

        fun create(path: Path, scope: CoroutineScope): ProfileRepository {
            require(path.name.endsWith(".preferences_pb")) {
                "DataStore path must end with .preferences_pb: $path"
            }
            return ProfileRepository(
                PreferenceDataStoreFactory.createWithPath(scope = scope, produceFile = { path })
            )
        }
    }
}
