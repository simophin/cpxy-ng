package dev.fanchao.cpxy.app

import androidx.datastore.preferences.core.mutablePreferencesOf
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

@OptIn(ExperimentalCoroutinesApi::class)
class ProfileInstanceManagerTest {
    @Test
    fun startsOnlyAfterLoadedAndClosesOnceOnConfigLossRestartAndClose() = runTest {
        val store = TestPreferencesDataStore(mutablePreferencesOf(ConfigJsonKey to "corrupt"))
        val job = SupervisorJob()
        val scope = CoroutineScope(job + UnconfinedTestDispatcher(testScheduler))
        val repository = ConfigRepository(store, json, scope)
        val client = RecordingNativeClient()
        val manager = ProfileInstanceManager(repository, client, NoOpLogger, scope)
        advanceUntilIdle()

        assertEquals(0, client.sessions.size)
        assertNull(manager.state.value.configUsed)

        store.replace(preferences(config("first")))
        advanceUntilIdle()
        assertEquals(1, client.sessions.size)

        store.replace(mutablePreferencesOf(ConfigJsonKey to "corrupt again"))
        advanceUntilIdle()
        assertEquals(1, client.sessions[0].closeCount)
        assertNull(manager.state.value.configUsed)

        store.replace(preferences(config("second")))
        advanceUntilIdle()
        assertEquals(2, client.sessions.size)

        manager.close()
        manager.close()
        assertEquals(1, client.sessions[1].closeCount)
        job.cancel()
    }

    @Test
    fun loadedConfigChangeRestartsAndClosesPreviousSessionOnce() = runTest {
        val store = TestPreferencesDataStore(preferences(config("first")))
        val job = SupervisorJob()
        val scope = CoroutineScope(job + UnconfinedTestDispatcher(testScheduler))
        val repository = ConfigRepository(store, json, scope)
        val client = RecordingNativeClient()
        val manager = ProfileInstanceManager(repository, client, NoOpLogger, scope)
        advanceUntilIdle()

        store.replace(preferences(config("second")))
        advanceUntilIdle()

        assertEquals(1, client.sessions[0].closeCount)
        assertEquals(0, client.sessions[1].closeCount)
        manager.close()
        job.cancel()
    }

    private fun preferences(config: ClientConfig) = mutablePreferencesOf(ConfigJsonKey to codec.encode(config))
    private fun config(id: String): ClientConfig {
        val profile = Profile(id, id, "https://example.test/$id", null, null)
        return DEFAULT_CLIENT_CONFIG.copy(profiles = listOf(profile), enabledProfileId = id)
    }

    private class RecordingNativeClient : NativeClient {
        val sessions = mutableListOf<RecordingSession>()
        override fun start(config: NativeClientConfig): NativeClientSession = RecordingSession().also(sessions::add)
    }

    private class RecordingSession : NativeClientSession {
        var closeCount = 0
        override fun close() { closeCount++ }
    }

    private object NoOpLogger : AppLogger {
        override fun debug(tag: String, message: String) = Unit
        override fun error(tag: String, message: String, throwable: Throwable?) = Unit
    }

    private companion object {
        val json = Json { ignoreUnknownKeys = true; isLenient = true }
        val codec = ConfigJsonCodec(json)
    }
}
