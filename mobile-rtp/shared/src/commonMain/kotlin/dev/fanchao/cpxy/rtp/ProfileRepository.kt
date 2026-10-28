package dev.fanchao.cpxy.rtp

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
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.decodeFromJsonElement
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import okio.Path
import kotlin.uuid.ExperimentalUuidApi
import kotlin.uuid.Uuid

@Serializable
data class StoredProfiles(
    val profiles: List<Profile> = emptyList(),
    /** The profile the connect button uses. */
    val selectedId: String? = null,
) {
    val selected: Profile? get() = profiles.firstOrNull { it.id == selectedId }
}

/** Keeps the profiles as one JSON value in a preferences DataStore. */
@OptIn(ExperimentalUuidApi::class)
class ProfileRepository(private val dataStore: DataStore<Preferences>) {
    val profiles: Flow<StoredProfiles> = dataStore.data.map { decode(it[ProfilesKey]) }

    /** Adds the profile, or replaces the one with its ID, and selects it. */
    suspend fun save(profile: Profile) = mutate { stored ->
        val index = stored.profiles.indexOfFirst { it.id == profile.id }
        val profiles = if (index >= 0) {
            stored.profiles.toMutableList().apply { this[index] = profile.copy(clientId = this[index].clientId) }
        } else {
            stored.profiles + profile.copy(clientId = Uuid.random().toString())
        }
        StoredProfiles(profiles, selectedId = profile.id)
    }

    /** DataStore serializes this transaction and completes the durable write before returning. */
    suspend fun connectionProfile(id: String): Profile {
        var result: Profile? = null
        mutate { stored ->
            val profile = stored.profiles.firstOrNull { it.id == id }
                ?: error("Select an existing profile")
            val identity = profile.clientId ?: Uuid.random().toString()
            require(isValidClientId(identity)) {
                "Saved client identity is invalid. Restore this profile's original identity or delete and recreate the profile."
            }
            val ready = profile.copy(clientId = identity)
            result = ready
            stored.copy(profiles = stored.profiles.map { if (it.id == id) ready else it })
        }
        return checkNotNull(result)
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

    private fun decode(raw: String?): StoredProfiles {
        if (raw == null) return StoredProfiles()
        val root = json.parseToJsonElement(raw).jsonObject
        val entries = root["profiles"]?.jsonArray ?: return json.decodeFromString(raw)
        val normalized = entries.map { entry ->
            val fields = entry.jsonObject
            val identity = fields["clientId"]
            // Only an absent field denotes a legacy identity. Preserve all malformed identities
            // as invalid (never as missing), including null, objects and non-string primitives.
            if (identity != null && (identity !is JsonPrimitive || !identity.isString)) {
                JsonObject(fields + ("clientId" to JsonPrimitive("")))
            } else entry
        }
        return json.decodeFromJsonElement(JsonObject(root + ("profiles" to JsonArray(normalized))))
    }

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

@OptIn(ExperimentalUuidApi::class)
fun isValidClientId(value: String): Boolean = runCatching {
    val uuid = Uuid.parse(value)
    uuid != Uuid.NIL && uuid.toString() == value
}.getOrDefault(false)
