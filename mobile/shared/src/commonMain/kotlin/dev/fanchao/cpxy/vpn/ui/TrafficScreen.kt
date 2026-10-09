package dev.fanchao.cpxy.vpn.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ArrowDownward
import androidx.compose.material3.ExtendedFloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withLink
import androidx.compose.ui.unit.dp
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.currentStateAsState
import dev.fanchao.cpxy.vpn.ConnectionEvent
import dev.fanchao.cpxy.vpn.VpnController
import dev.fanchao.cpxy.vpn.VpnState
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import kotlin.time.Duration.Companion.milliseconds

/**
 * The connections made while the screen is shown, as a log: the newest at the bottom, followed
 * until the user scrolls away from it. Leaving the screen, or the app, drops the log and stops
 * the engine reporting connections.
 */
@Composable
internal fun TrafficScreen(
    controller: VpnController,
    contentPadding: PaddingValues,
    modifier: Modifier = Modifier,
) {
    val lifecycleState by LocalLifecycleOwner.current.lifecycle.currentStateAsState()
    if (lifecycleState.isAtLeast(Lifecycle.State.STARTED)) {
        TrafficLogView(controller, contentPadding, modifier)
    }
}

@Composable
private fun TrafficLogView(
    controller: VpnController,
    contentPadding: PaddingValues,
    modifier: Modifier,
) {
    val vpnState by controller.state.collectAsState()
    val log = remember { TrafficLog() }
    val listState = rememberLazyListState()
    val scope = rememberCoroutineScope()
    // Whether to keep the newest connection in view as more arrive.
    var following by remember { mutableStateOf(true) }
    // The attribution row comes before the entries.
    val lastIndex = { log.entries.size }

    LaunchedEffect(controller) {
        val pending = Channel<ConnectionEvent>(Channel.UNLIMITED)
        launch(Dispatchers.Default) { controller.connections.collect(pending::send) }
        while (true) {
            // Batched, so a burst of connections recomposes and scrolls once.
            val batch = mutableListOf(pending.receive())
            delay(BATCH_DELAY)
            while (true) batch += pending.tryReceive().getOrNull() ?: break
            log.append(batch)
            // In its own coroutine: a user's scroll cancels it, which must not end this loop.
            if (following) launch { listState.scrollToItem(lastIndex()) }
        }
    }

    val followWhenAtEnd = remember(listState) {
        object : NestedScrollConnection {
            // Scrolling from code bypasses nested scrolling, so this only sees the user's drags
            // and flings.
            override fun onPostScroll(consumed: Offset, available: Offset, source: NestedScrollSource): Offset {
                following = !listState.canScrollForward
                return Offset.Zero
            }
        }
    }

    Box(modifier = modifier.fillMaxSize()) {
        LazyColumn(
            modifier = Modifier.fillMaxSize().nestedScroll(followWhenAtEnd),
            state = listState,
            contentPadding = contentPadding,
        ) {
            item(key = "attribution") { Attribution() }
            if (log.entries.isEmpty()) {
                item(key = "empty") {
                    Text(
                        if (vpnState is VpnState.Connected) "Waiting for connections…" else "Connect to see the traffic.",
                        modifier = Modifier.fillMaxWidth().padding(16.dp),
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            items(log.entries, key = { it.id }) { ConnectionRow(it.event) }
        }

        AnimatedVisibility(
            visible = !following,
            enter = fadeIn(),
            exit = fadeOut(),
            modifier = Modifier
                .align(Alignment.BottomCenter)
                .padding(bottom = contentPadding.calculateBottomPadding() + 16.dp),
        ) {
            ExtendedFloatingActionButton(
                onClick = {
                    following = true
                    scope.launch { listState.animateScrollToItem(lastIndex()) }
                },
                icon = { Icon(Icons.Default.ArrowDownward, contentDescription = null) },
                text = { Text("Latest") },
            )
        }
    }
}

@Composable
private fun Attribution() {
    val text = buildAnnotatedString {
        append("Countries: ")
        withLink(LinkAnnotation.Url("https://db-ip.com")) { append("IP Geolocation by DB-IP") }
    }
    Text(
        text,
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
        style = MaterialTheme.typography.labelSmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
}

@Composable
private fun ConnectionRow(event: ConnectionEvent) {
    Row(
        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 6.dp),
        horizontalArrangement = Arrangement.spacedBy(12.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            flagEmoji(event.countryCode),
            modifier = Modifier.semantics { contentDescription = event.countryCode ?: "Unknown country" },
            style = MaterialTheme.typography.titleLarge,
        )
        Column(modifier = Modifier.weight(1f)) {
            Text(
                "${event.host}:${event.port}",
                style = MaterialTheme.typography.bodyMedium,
                maxLines = 1,
                overflow = TextOverflow.Ellipsis,
            )
            val detail = event.error?.let { "${event.outbound} failed: $it" }
                ?: "${event.outbound}, ${event.delayMillis} ms"
            Text(
                detail,
                style = MaterialTheme.typography.bodySmall,
                color = if (event.error != null) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurfaceVariant,
                maxLines = 2,
                overflow = TextOverflow.Ellipsis,
            )
        }
    }
}

private val BATCH_DELAY = 100.milliseconds
