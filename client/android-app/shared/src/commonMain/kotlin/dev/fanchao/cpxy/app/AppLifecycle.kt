package dev.fanchao.cpxy.app

import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import io.ktor.client.HttpClient
import kotlinx.atomicfu.atomic
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.cancel

@SingleIn(AppScope::class)
class AppLifecycle internal constructor(
    private val closeResources: () -> Unit,
    private val applicationScope: CoroutineScope,
) {
    @Inject
    constructor(
        profileInstanceManager: ProfileInstanceManager,
        httpClient: HttpClient,
        applicationScope: CoroutineScope,
    ) : this(
        closeResources = {
            profileInstanceManager.close()
            httpClient.close()
        },
        applicationScope = applicationScope,
    )

    private val closed = atomic(false)

    fun close() {
        if (!closed.compareAndSet(expect = false, update = true)) return
        try {
            closeResources()
        } finally {
            applicationScope.cancel()
        }
    }
}
