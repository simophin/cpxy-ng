package dev.fanchao.cpxy

import com.sun.jna.Pointer
import kotlinx.coroutines.DelicateCoroutinesApi
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.GlobalScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.flowOf
import kotlinx.coroutines.flow.scan
import kotlinx.coroutines.flow.stateIn

@OptIn(ExperimentalCoroutinesApi::class)
class ClientInstanceManager (
    repository: ClientConfigurationRepository,
    clientProvider: () -> Client,
) {
    private val mutableStarted = MutableStateFlow(false)

    val started: StateFlow<Boolean>
        get() = mutableStarted

    @OptIn(DelicateCoroutinesApi::class)
    val state: StateFlow<Map<String, InstanceState>> = mutableStarted
        .flatMapLatest { started ->
            if (!started) flowOf(emptyList())
            else repository.configurations
        }
        .scan(emptyMap<String, InstanceState>()) { acc, configurations ->
            val keep = acc.filter { existing ->
                val newConfig = configurations.firstOrNull { it.id == existing.key }
                if (newConfig == null || existing.value.needsRecreate(newConfig)) {
                    existing.value.instance.getOrNull()?.let(clientProvider()::destroy)
                    false
                } else {
                    true
                }
            }

            configurations.associate { newConfig ->
                val existing = keep[newConfig.id]
                if (existing != null) {
                    newConfig.id to existing
                } else {
                    val instance = runCatching {
                        clientProvider().create(
                            serverHost = newConfig.serverHost,
                            serverPort = newConfig.serverPort,
                            key = newConfig.key,
                            bindAddress = newConfig.bindAddress
                        )
                    }

                    newConfig.id to InstanceState(instance, newConfig)
                }
            }
        }
        .stateIn(GlobalScope, SharingStarted.Eagerly, emptyMap())

    fun start() {
        mutableStarted.value = true
    }

    fun stop() {
        mutableStarted.value = false
    }

    data class InstanceState(
        val instance: Result<Pointer>,
        val serverHost: String,
        val serverPort: UShort,
        val key: String,
        val bindAddress: String,
    ) {
        constructor(instance: Result<Pointer>, config: ClientConfiguration): this(
            instance = instance,
            serverHost = config.serverHost,
            serverPort = config.serverPort,
            key = config.key,
            bindAddress = config.bindAddress
        )

        fun needsRecreate(config: ClientConfiguration): Boolean {
            return serverHost != config.serverHost ||
                serverPort != config.serverPort ||
                key != config.key ||
                bindAddress != config.bindAddress
        }
    }
}