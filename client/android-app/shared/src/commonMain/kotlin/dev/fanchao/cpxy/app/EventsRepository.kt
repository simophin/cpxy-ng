package dev.fanchao.cpxy.app

import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import io.ktor.client.HttpClient
import io.ktor.client.plugins.websocket.webSocket
import io.ktor.websocket.Frame
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.channelFlow
import kotlinx.coroutines.flow.flatMapLatest
import kotlinx.coroutines.flow.mapNotNull
import kotlinx.coroutines.flow.shareIn
import kotlinx.serialization.ExperimentalSerializationApi
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonClassDiscriminator

@Inject
@SingleIn(AppScope::class)
class EventsRepository(
    manager: ProfileInstanceManager,
    client: HttpClient,
    private val json: Json,
    private val logger: AppLogger,
    applicationScope: CoroutineScope,
) {
    @OptIn(ExperimentalCoroutinesApi::class)
    val events: SharedFlow<Event> = manager.state
        .mapNotNull { it.startedApiServerPort }
        .flatMapLatest { port ->
            channelFlow {
                while (true) {
                    try {
                        client.webSocket("ws://127.0.0.1:$port/events") {
                            for (frame in incoming) {
                                if (frame !is Frame.Text) break
                                val event = try {
                                    json.decodeFromString<Event>(frame.data.toString(Charsets.UTF_8))
                                } catch (e: Exception) {
                                    logger.error(TAG, "Error decoding event", e)
                                    continue
                                }
                                logger.debug(TAG, "Received event: $event")
                                send(event)
                            }
                        }
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Exception) {
                        logger.error(TAG, "Error in websocket", e)
                        delay(1_000)
                    }
                }
            }
        }
        .shareIn(applicationScope, SharingStarted.Eagerly, replay = 100)

    @OptIn(ExperimentalSerializationApi::class)
    @JsonClassDiscriminator(discriminator = "type")
    @Serializable
    sealed interface Event {
        @Serializable @SerialName("Connected")
        data class Connected(
            val host: String, val port: UShort, val outbound: String,
            @SerialName("delay_mills") val delayMills: Long,
            @SerialName("request_time_mills") val requestTimeEpochMs: Long,
        ) : Event

        @Serializable @SerialName("Error")
        data class Error(
            val host: String, val port: UShort, val outbound: String,
            @SerialName("delay_mills") val delayMills: Long,
            @SerialName("request_time_mills") val requestTimeEpochMs: Long,
            val error: String,
        ) : Event
    }

    private companion object { const val TAG = "EventsRepository" }
}

internal val ProfileInstanceManager.RunningState.startedApiServerPort: UShort?
    get() = configUsed?.apiServerPort?.takeIf { startedResult?.isSuccess == true }
