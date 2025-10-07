package dev.fanchao.cpxy.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.App.Companion.appInstance
import dev.fanchao.cpxy.EventsRepository
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import java.text.NumberFormat
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import kotlin.math.absoluteValue
import kotlin.time.Clock
import kotlin.time.Duration.Companion.milliseconds
import kotlin.time.ExperimentalTime
import kotlin.time.Instant
import kotlin.time.toJavaInstant

@OptIn(ExperimentalMaterial3Api::class, ExperimentalTime::class)
@Composable
fun EventViewer(
    modifier: Modifier = Modifier
) {
    val repo = LocalContext.current.appInstance.eventsRepository

    val list = remember {
        mutableStateListOf<EventsRepository.Event>()
    }

    val state = rememberLazyListState()

    LaunchedEffect(Unit) {
        val buffer = mutableListOf<EventsRepository.Event>()
        var flushDeadline: Instant? = null

        repo.events.collectLatest { event ->
            buffer += event

            val now = Clock.System.now()
            if (flushDeadline == null) {
                flushDeadline = now + 100.milliseconds
            }

            if (now < flushDeadline) {
                delay(flushDeadline - now)
            }

            list.addAll(buffer)
            buffer.clear()

            // Are we scrolled to the bottom? If so keep at it
            if (!state.canScrollForward) {
                state.animateScrollToItem(list.size)
            }
        }
    }

    LazyColumn(
        modifier = modifier.fillMaxSize(),
        state = state,
    ) {
        items(list) { item ->
            val badgeText: String
            val text: String
            val time: Instant
            val delayMills: Long
            val isError: Boolean

            when (item) {
                is EventsRepository.Event.Connected -> {
                    badgeText = item.outbound
                    text = "${item.host}:${item.port}"
                    time = Instant.fromEpochMilliseconds(item.requestTimeEpochMs)
                    delayMills = item.delayMills
                    isError = false
                }

                is EventsRepository.Event.Error -> {
                    badgeText = item.outbound
                    text = "${item.host}:${item.port}"
                    time = Instant.fromEpochMilliseconds(item.requestTimeEpochMs)
                    delayMills = item.delayMills
                    isError = true
                }
            }

            Row(
                modifier = Modifier
                    .fillMaxWidth()
                    .clickable {}
                    .padding(8.dp),
                horizontalArrangement = Arrangement.spacedBy(4.dp),
                verticalAlignment = Alignment.Top
            ) {
                if (isError) {
                    Text(
                        "Error",
                        style = MaterialTheme.typography.labelSmall,
                        maxLines = 1,
                        color = Color.White,
                        modifier = Modifier
                            .background(
                                color = Color.Red,
                                shape = RoundedCornerShape(4.dp)
                            )
                            .padding(vertical = 2.dp, horizontal = 4.dp)
                    )

                }

                val backgroundColor = COLORS[badgeText.hashCode().absoluteValue % COLORS.size]
                Text(
                    badgeText,
                    style = MaterialTheme.typography.labelSmall,
                    maxLines = 1,
                    color = Color.White,
                    modifier = Modifier
                        .background(
                            color = backgroundColor,
                            shape = RoundedCornerShape(4.dp)
                        )
                        .padding(vertical = 2.dp, horizontal = 4.dp)
                )

                Text(
                    NumberFormat.getNumberInstance().format(delayMills) + "ms",
                    maxLines = 1,
                    style = MaterialTheme.typography.labelSmall,
                    color = Color.White,
                    modifier = Modifier
                        .background(
                            color = Color.DarkGray.copy(alpha = 0.3f),
                            shape = RoundedCornerShape(4.dp)
                        )
                        .padding(vertical = 2.dp, horizontal = 4.dp)
                )

                Text(
                    text,
                    style = MaterialTheme.typography.bodySmall,
                    maxLines = 2,
                    overflow = TextOverflow.Ellipsis,
                    modifier = Modifier.weight(1f)
                )

                Text(
                    text = time.toJavaInstant().atZone(ZoneId.systemDefault()).format(
                        DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT)
                    ),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.secondary,
                )
            }
        }

        if (list.isEmpty()) {
            item {
                Text("No events yet", modifier = Modifier
                    .fillMaxWidth()
                    .padding(8.dp))
            }
        }
    }

}

private val COLORS = listOf(
    Color.Blue,
    Color.Cyan,
    Color.Green,
    Color.Magenta,
)