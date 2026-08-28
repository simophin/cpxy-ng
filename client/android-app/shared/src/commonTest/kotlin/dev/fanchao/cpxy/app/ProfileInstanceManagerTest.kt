package dev.fanchao.cpxy.app

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.Json
import kotlin.test.Test
import kotlin.test.assertEquals

class ProfileInstanceManagerTest {
    @OptIn(ExperimentalCoroutinesApi::class)
    @Test
    fun replacingAndClosingAProfileClosesItsNativeSession() = runTest {
        val applicationJob = SupervisorJob()
        val applicationScope = CoroutineScope(applicationJob + UnconfinedTestDispatcher(testScheduler))
        val repository = ConfigRepository(InMemoryPersistence(), Json, applicationScope)
        val client = RecordingNativeClient()
        val manager = ProfileInstanceManager(repository, client, NoOpLogger, applicationScope)
        val first = profile("first")
        val second = profile("second")

        repository.saveProfile(first)
        repository.saveProfile(second)
        repository.setProfileEnabled(first.id)
        advanceUntilIdle()
        repository.setProfileEnabled(second.id)
        advanceUntilIdle()

        assertEquals(1, client.sessions[0].closeCount)
        assertEquals(0, client.sessions[1].closeCount)

        manager.close()
        assertEquals(1, client.sessions[1].closeCount)
        applicationJob.cancel()
    }

    private fun profile(id: String) = Profile(
        id = id,
        name = id,
        mainServerUrl = "https://example.com/$id",
        aiServerUrl = null,
        tailscaleServerUrl = null,
    )

    private class InMemoryPersistence : ConfigPersistence {
        override fun load(): String? = null
        override fun save(value: String) = Unit
    }

    private class RecordingNativeClient : NativeClient {
        val sessions = mutableListOf<RecordingSession>()
        override fun start(config: NativeClientConfig): NativeClientSession =
            RecordingSession().also(sessions::add)
    }

    private class RecordingSession : NativeClientSession {
        var closeCount = 0
        override fun close() { closeCount++ }
    }

    private object NoOpLogger : AppLogger {
        override fun debug(tag: String, message: String) = Unit
        override fun error(tag: String, message: String, throwable: Throwable?) = Unit
    }
}
