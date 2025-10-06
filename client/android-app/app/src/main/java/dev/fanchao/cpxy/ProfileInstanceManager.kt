package dev.fanchao.cpxy

import android.util.Log
import android.widget.Toast
import com.sun.jna.Pointer
import kotlinx.coroutines.DelicateCoroutinesApi
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.GlobalScope
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.scan
import kotlinx.coroutines.flow.stateIn

@OptIn(ExperimentalCoroutinesApi::class)
class ProfileInstanceManager (
    repository: ConfigRepository,
    clientProvider: () -> Client,
) {
    data class RunningState(
        val configUsed: ClientConfig? = null,
        val startedResult: Result<Pointer>? = null,
    )

    @OptIn(DelicateCoroutinesApi::class)
    val state: StateFlow<RunningState> = repository
        .clientConfig
        .scan(RunningState()) { acc, newConfig ->
            acc.startedResult
                ?.getOrNull()
                ?.let { clientProvider().destroy_client(it) }

            RunningState(
                configUsed = newConfig,
                startedResult = newConfig.enabledProfile
                    ?.let { profile ->
                        runCatching {
                            clientProvider().create(
                                httpProxyPort = newConfig.httpProxyPort,
                                socks5ProxyPort = newConfig.socks5ProxyPort,
                                mainServerUrl = profile.mainServerUrl,
                                aiServerUrl = profile.aiServerUrl,
                                tailscaleServerUrl = profile.tailscaleServerUrl,
                                apiServerPort = newConfig.apiServerPort,
                            )
                        }.onFailure {
                            Log.e("ProfileInstanceManager", "Failed to start client for profile $profile", it)
                        }
                    }
            )
        }
        .stateIn(GlobalScope, SharingStarted.Eagerly, RunningState())


}