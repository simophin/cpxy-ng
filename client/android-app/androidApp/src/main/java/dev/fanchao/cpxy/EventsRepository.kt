package dev.fanchao.cpxy

import android.util.Log
import io.ktor.client.HttpClient
import io.ktor.client.plugins.websocket.webSocket
import io.ktor.websocket.Frame
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.GlobalScope
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

class EventsRepository(
    manager: ProfileInstanceManager,
    client: HttpClient,
    json: Json,
) {
    @OptIn(ExperimentalCoroutinesApi::class)
    val events: SharedFlow<Event> = manager
        .state
        .mapNotNull { it.configUsed?.apiServerPort }
        .flatMapLatest { port ->
            channelFlow {
                while (true) {
                    try {
                        client.webSocket("ws://127.0.0.1:$port/events") {
                            while (true) {
                                when (val frame = incoming.receive()) {
                                    is Frame.Text -> {
                                        val event = try {
                                            val text = frame.data.toString(Charsets.UTF_8)
                                            json.decodeFromString<Event>(text)
                                        } catch (e: Exception) {
                                            Log.e("EventsRepository", "Error decoding event", e)
                                            continue
                                        }

                                        Log.d("EventsRepository", "Received event: $event")

                                        this@channelFlow.send(event)
                                    }

                                    else -> break
                                }

                            }
                        }
                    } catch (e: CancellationException) {
                        throw e
                    } catch (e: Exception) {
                        Log.e("EventsRepository", "Error in websocket", e)
                        delay(1000)
                    }
                }
            }
        }
        .shareIn(GlobalScope, SharingStarted.Eagerly, replay = 100)

    @OptIn(ExperimentalSerializationApi::class)
    @JsonClassDiscriminator(discriminator = "type")
    @Serializable
    sealed interface Event {

        @Serializable
        @SerialName("Connected")
        data class Connected(
            val host: String,
            val port: UShort,
            val outbound: String,
            @SerialName("delay_mills")
            val delayMills: Long,
            @SerialName("request_time_mills")
            val requestTimeEpochMs: Long,
        ) : Event

        @Serializable
        @SerialName("Error")
        data class Error(
            val host: String,
            val port: UShort,
            val outbound: String,
            @SerialName("delay_mills")
            val delayMills: Long,
            @SerialName("request_time_mills")
            val requestTimeEpochMs: Long,
            val error: String,
        ) : Event
    }
}