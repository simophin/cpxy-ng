package dev.fanchao.cpxy.app

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class EventsRepositoryTest {
    @Test
    fun exposesApiServerPortOnlyAfterClientStartsSuccessfully() {
        val config = DEFAULT_CLIENT_CONFIG.copy(
            profiles = listOf(Profile("profile", "Profile", "https://example.test", null, null)),
            enabledProfileId = "profile",
        )
        val session = object : NativeClientSession {
            override fun close() = Unit
        }

        assertEquals(
            config.apiServerPort,
            ProfileInstanceManager.RunningState(config, Result.success(session)).startedApiServerPort,
        )
        assertNull(
            ProfileInstanceManager.RunningState(
                config,
                Result.failure(IllegalStateException("start failed")),
            ).startedApiServerPort,
        )
        assertNull(ProfileInstanceManager.RunningState(config).startedApiServerPort)
        assertNull(ProfileInstanceManager.RunningState().startedApiServerPort)
    }
}
