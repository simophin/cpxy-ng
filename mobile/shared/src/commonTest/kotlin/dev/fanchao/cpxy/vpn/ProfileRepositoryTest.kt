package dev.fanchao.cpxy.vpn

import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import okio.FileSystem
import kotlin.random.Random
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class ProfileRepositoryTest {
    private val directory = FileSystem.SYSTEM_TEMPORARY_DIRECTORY / "cpxy-vpn-test-${Random.nextLong().toULong()}"
    private val scope = CoroutineScope(Dispatchers.Default + Job())
    private val repository = ProfileRepository.create(directory / "profiles.preferences_pb", scope)

    @AfterTest
    fun cleanUp() {
        scope.cancel()
        FileSystem.SYSTEM.deleteRecursively(directory)
    }

    private fun profile(id: String) = Profile(id, "Profile $id", "https://:k@$id.example.com", listOf("223.5.5.5"), listOf("8.8.8.8"))

    @Test
    fun profilesSavedWithAnIpv6ModeStillLoad() = runTest {
        val dataStore = PreferenceDataStoreFactory.createWithPath(scope = scope, produceFile = { directory / "old.preferences_pb" })
        dataStore.edit {
            it[stringPreferencesKey("profiles_json")] =
                """{"selectedId":"a","profiles":[{"id":"a","name":"Profile a","server":"https://:k@a.example.com","dnsUpstream":["223.5.5.5"],"dnsAlternative":["8.8.8.8"],"ipv6":"direct"}]}"""
        }
        assertEquals(StoredProfiles(listOf(profile("a")), "a"), ProfileRepository(dataStore).profiles.first())
    }

    @Test
    fun startsEmpty() = runTest {
        assertEquals(StoredProfiles(), repository.profiles.first())
    }

    @Test
    fun savingAddsReplacesAndSelects() = runTest {
        repository.save(profile("a"))
        repository.save(profile("b"))
        repository.save(profile("a").copy(name = "Renamed"))

        val stored = repository.profiles.first()
        assertEquals(listOf("Renamed", "Profile b"), stored.profiles.map { it.name })
        assertEquals("a", stored.selectedId)
    }

    @Test
    fun deletingTheSelectedProfileSelectsAnother() = runTest {
        repository.save(profile("a"))
        repository.save(profile("b"))
        repository.delete("b")
        assertEquals("a", repository.profiles.first().selectedId)

        repository.delete("a")
        val stored = repository.profiles.first()
        assertEquals(emptyList(), stored.profiles)
        assertNull(stored.selectedId)
    }

    @Test
    fun selects() = runTest {
        repository.save(profile("a"))
        repository.save(profile("b"))
        repository.select("a")
        assertEquals("a", repository.profiles.first().selected?.id)
    }
}
