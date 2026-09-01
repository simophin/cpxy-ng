package dev.fanchao.cpxy.app

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.launch
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse

class AppLifecycleTest {
    @Test
    fun closeIsIdempotentAndCancelsOwnedScope() {
        val job = SupervisorJob()
        val scope = CoroutineScope(job)
        var closeCount = 0
        val child = scope.launch { Job().join() }
        val lifecycle = AppLifecycle(
            closeResources = { closeCount++ },
            applicationScope = scope,
        )

        lifecycle.close()
        lifecycle.close()

        assertEquals(1, closeCount)
        assertFalse(job.isActive)
        assertFalse(child.isActive)
    }
}
