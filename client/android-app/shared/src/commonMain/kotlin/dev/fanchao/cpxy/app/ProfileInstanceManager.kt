package dev.fanchao.cpxy.app

import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import kotlinx.atomicfu.atomic
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.launch

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

    private val activeSession = atomic<NativeClientSession?>(null)
    private val closed = atomic(false)
    private val mutableState = MutableStateFlow(RunningState())
    val state: StateFlow<RunningState> = mutableState

    init {
        applicationScope.launch {
            repository.loadState.collect { loadState ->
                if (closed.value) return@collect
                activeSession.getAndSet(null)?.close()
                mutableState.value = when (loadState) {
                    ConfigLoadState.Loading, is ConfigLoadState.Error -> RunningState()
                    is ConfigLoadState.Loaded -> start(loadState.config)
                }
            }
        }
    }

    private fun start(config: ClientConfig): RunningState {
        val result = config.enabledProfile?.let { profile ->
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
                ).also { session ->
                    activeSession.value = session
                    if (closed.value) activeSession.getAndSet(null)?.close()
                }
            }.onFailure {
                logger.error(TAG, "Failed to start client for profile $profile", it)
            }
        }
        return RunningState(configUsed = config, startedResult = result)
    }

    fun close() {
        closed.value = true
        activeSession.getAndSet(null)?.close()
        mutableState.value = RunningState()
    }

    private companion object { const val TAG = "ProfileInstanceManager" }
}
