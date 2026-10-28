package dev.fanchao.cpxy.rtp

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancel
import kotlinx.coroutines.cancelAndJoin
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import okio.FileSystem
import kotlin.random.Random
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull
import kotlin.test.assertTrue
import kotlin.test.assertFailsWith
import kotlin.test.assertNotEquals

class ProfileRepositoryTest {
    private val directory = FileSystem.SYSTEM_TEMPORARY_DIRECTORY / "cpxy-rtp-test-${Random.nextLong().toULong()}"
    private val scope = CoroutineScope(Dispatchers.Default + Job())
    private val repository = ProfileRepository.create(directory / "profiles.preferences_pb", scope)

    @AfterTest
    fun cleanUp() {
        scope.cancel()
        FileSystem.SYSTEM.deleteRecursively(directory)
    }

    private fun profile(id: String) = Profile(id, "Profile $id", "$id.example.com:30000")

    @Test
    fun identityIsDurableAndEditsCannotReplaceIt() = runTest {
        repository.save(profile("a"))
        val attempts = List(12) { async(Dispatchers.Default) { repository.connectionProfile("a") } }.awaitAll()
        val identity = attempts.first().clientId
        assertTrue(isValidClientId(identity!!))
        assertEquals(setOf(identity), attempts.map { it.clientId }.toSet())
        repository.save(profile("a").copy(endpoint = "changed:123", clientId = "bad"))
        assertEquals(identity, repository.connectionProfile("a").clientId)
        repository.save(attempts.first().copy(id = "copy"))
        assertNotEquals(identity, repository.connectionProfile("copy").clientId)
    }

    @Test
    fun identitySurvivesClosingAndReopeningDurableStorage() = runTest {
        repository.save(profile("a"))
        val original = repository.connectionProfile("a")
        scope.coroutineContext[Job]!!.cancelAndJoin()
        val restartedScope = CoroutineScope(Dispatchers.Default + Job())
        try {
            val reopened = ProfileRepository.create(directory / "profiles.preferences_pb", restartedScope)
            assertEquals(original, reopened.connectionProfile("a"))
            assertEquals("a", reopened.profiles.first().selectedId)
        } finally { restartedScope.coroutineContext[Job]!!.cancelAndJoin() }
    }

    @Test
    fun migratesLegacyJsonWithoutChangingKeyIdSelectionOrDns() = runTest {
        val data = PreferenceDataStoreFactory.createWithPath(scope = scope) {
            directory / "legacy.preferences_pb"
        }
        data.edit {
            it[stringPreferencesKey("profiles_json")] = """{"profiles":[{"id":"key-id","name":"Legacy","endpoint":"[::1]:30000","address":"10.88.0.9","dnsUpstream":["223.5.5.5"],"dnsAlternative":["9.9.9.9"]}],"selectedId":"key-id"}"""
        }
        val repo = ProfileRepository(data)
        val migrated = repo.connectionProfile("key-id")
        assertEquals("key-id", repo.profiles.first().selectedId)
        assertEquals("Legacy", migrated.name)
        assertEquals("[::1]:30000", migrated.endpoint)
        assertEquals(listOf("9.9.9.9"), migrated.dnsAlternative)
        assertEquals(Profile.DefaultServerIdentity, migrated.serverIdentity)
        assertTrue(isValidClientId(migrated.clientId!!))
        assertEquals(migrated, repo.connectionProfile("key-id"))
        assertTrue(!migrated.engineProfileJson().contains("address"))
        data.edit {
            it[stringPreferencesKey("profiles_json")] = """{"profiles":[{"id":"key-id","name":"Legacy","endpoint":"a:1","clientId":"broken"}],"selectedId":"key-id"}"""
        }
        assertFailsWith<IllegalArgumentException> { repo.connectionProfile("key-id") }
        assertEquals("broken", repo.profiles.first().selected?.clientId)
        for (identity in listOf("null", "{}", "[]", "true", "123", "\"\"", "\"00000000-0000-0000-0000-000000000000\"")) {
            data.edit {
                it[stringPreferencesKey("profiles_json")] = """{"profiles":[{"id":"key-id","name":"Legacy","endpoint":"a:1","clientId":$identity}]}"""
            }
            assertFailsWith<IllegalArgumentException> { repo.connectionProfile("key-id") }
        }

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
