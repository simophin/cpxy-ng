package dev.fanchao.cpxy.app

import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.scan
import kotlinx.coroutines.flow.stateIn

@Inject
@SingleIn(AppScope::class)
class ProfileInstanceManager(
    repository: ConfigRepository,
    private val nativeClient: NativeClient,
    private val logger: AppLogger,
    applicationScope: CoroutineScope,
) {
    data class RunningState(
        val configUsed: ClientConfig? = null,
        val startedResult: Result<NativeClientSession>? = null,
    )

    @OptIn(ExperimentalCoroutinesApi::class)
    val state: StateFlow<RunningState> = repository.clientConfig
        .scan(RunningState()) { previous, config ->
            previous.startedResult?.getOrNull()?.close()
            RunningState(
                configUsed = config,
                startedResult = config.enabledProfile?.let { profile ->
                    runCatching {
                        nativeClient.start(
                            NativeClientConfig(
                                httpProxyPort = config.httpProxyPort,
                                socks5ProxyPort = config.socks5ProxyPort,
                                apiServerPort = config.apiServerPort,
                                dnsServer = config.dnsServer,
                                mainServerUrl = profile.mainServerUrl,
                                aiServerUrl = profile.aiServerUrl,
                                tailscaleServerUrl = profile.tailscaleServerUrl,
                            ),
                        )
                    }.onFailure {
                        logger.error(TAG, "Failed to start client for profile $profile", it)
                    }
                },
            )
        }
        .stateIn(applicationScope, SharingStarted.Eagerly, RunningState())

    fun close() {
        state.value.startedResult?.getOrNull()?.close()
    }

    private companion object { const val TAG = "ProfileInstanceManager" }
}
