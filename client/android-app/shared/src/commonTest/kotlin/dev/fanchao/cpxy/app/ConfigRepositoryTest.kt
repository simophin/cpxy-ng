package dev.fanchao.cpxy.app

import androidx.datastore.preferences.core.mutablePreferencesOf
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.async
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertIs

@OptIn(ExperimentalCoroutinesApi::class)
class ConfigRepositoryTest {
    @Test
    fun missingValueLoadsValidatedDefault() = runTest {
        val store = TestPreferencesDataStore()
        val job = SupervisorJob()
        val repository = ConfigRepository(store, json, CoroutineScope(job + StandardTestDispatcher(testScheduler)))
        assertIs<ConfigLoadState.Loading>(repository.loadState.value)
        advanceUntilIdle()

        assertEquals(DEFAULT_CLIENT_CONFIG, assertIs<ConfigLoadState.Loaded>(repository.loadState.value).config)
        job.cancel()
    }

    @Test
    fun corruptJsonIsAnErrorAndALaterEmissionRecovers() = runTest {
        val store = TestPreferencesDataStore(mutablePreferencesOf(ConfigJsonKey to "not-json"))
        val (repository, job) = repository(store)
        advanceUntilIdle()
        assertIs<ConfigLoadState.Error>(repository.loadState.value)

        store.replace(mutablePreferencesOf(ConfigJsonKey to codec.encode(DEFAULT_CLIENT_CONFIG)))
        advanceUntilIdle()

        assertEquals(DEFAULT_CLIENT_CONFIG, assertIs<ConfigLoadState.Loaded>(repository.loadState.value).config)
        job.cancel()
    }

    @Test
    fun concurrentMutationsAreAtomicAndDoNotLoseProfiles() = runTest {
        val store = TestPreferencesDataStore()
        val (repository, job) = repository(store)
        advanceUntilIdle()

        val first = async { repository.saveProfile(profile("first")) }
        val second = async { repository.saveProfile(profile("second")) }
        first.await()
        second.await()
        advanceUntilIdle()

        assertEquals(setOf("first", "second"), loaded(repository).profiles.map { it.id }.toSet())
        job.cancel()
    }

    @Test
    fun validationAndStorageErrorsAreReturnedToCaller() = runTest {
        val store = TestPreferencesDataStore()
        val (repository, job) = repository(store)
        advanceUntilIdle()

        assertFailsWith<IllegalArgumentException> { repository.saveProfile(profile("bad").copy(name = "")) }
        assertEquals(null, store.current[ConfigJsonKey])
        store.updateFailure = IllegalStateException("disk failed")
        assertFailsWith<IllegalStateException> { repository.setProfileEnabled(null) }
        job.cancel()
    }

    private fun repository(store: TestPreferencesDataStore): Pair<ConfigRepository, Job> {
        val job = SupervisorJob()
        val scope = CoroutineScope(job + UnconfinedTestDispatcher())
        return ConfigRepository(store, json, scope) to job
    }

    private fun loaded(repository: ConfigRepository) = assertIs<ConfigLoadState.Loaded>(repository.loadState.value).config
    private fun profile(id: String) = Profile(id, id, "https://example.test/$id", null, null)

    private companion object {
        val json = Json { ignoreUnknownKeys = true; isLenient = true }
        val codec = ConfigJsonCodec(json)
    }
}
