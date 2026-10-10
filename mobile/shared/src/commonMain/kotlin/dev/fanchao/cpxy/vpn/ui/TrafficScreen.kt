package dev.fanchao.cpxy.vpn.ui

import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.calculateEndPadding
import androidx.compose.foundation.layout.calculateStartPadding
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
import androidx.compose.runtime.snapshotFlow
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.input.nestedscroll.NestedScrollConnection
import androidx.compose.ui.input.nestedscroll.NestedScrollSource
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.platform.LocalLayoutDirection
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
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlin.time.Duration.Companion.milliseconds

/**
 * The engine's recent connections, as a log: the newest at the bottom, followed and polled for
 * until the user scrolls away from it. Scrolling up loads older ones. Nothing is polled while the
 * screen, or the app, is not shown.
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
    val log = remember(controller) { TrafficLog(controller) }
    val listState = rememberLazyListState()
    val scope = rememberCoroutineScope()
    // Whether to keep the newest connection in view, and poll for more.
    var following by remember { mutableStateOf(true) }
    val lastIndex = { (log.entries.size - 1).coerceAtLeast(0) }

    LaunchedEffect(log) {
        snapshotFlow { following }.collectLatest { following ->
            while (following) {
                log.loadNewer()
                // In its own coroutine: a user's scroll cancels it, which must not end this loop.
                launch { listState.scrollToItem(lastIndex()) }
                delay(POLL_INTERVAL)
            }
        }
    }

    LaunchedEffect(log) {
        while (true) {
            // Checked afresh after each page: the list may still be near the oldest entry.
            snapshotFlow { log.hasOlder && listState.firstVisibleItemIndex < LOAD_OLDER_WITHIN }.first { it }
            val inserted = log.loadOlder()
            // The list only follows its first visible entry by key when it moved a little, so it
            // is kept in place here. Requested before the next layout, so nothing jumps.
            listState.requestScrollToItem(
                listState.firstVisibleItemIndex + inserted,
                listState.firstVisibleItemScrollOffset,
            )
            // Until the list lays the new entries out, it still looks near the oldest.
            snapshotFlow { listState.layoutInfo.totalItemsCount }.first { it >= log.entries.size }
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

    val layoutDirection = LocalLayoutDirection.current
    Column(modifier = modifier.fillMaxSize().padding(top = contentPadding.calculateTopPadding())) {
        // Outside the list: as its first item, it would stay in view while older entries are
        // inserted after it, so they would keep loading.
        Attribution()
        Box(modifier = Modifier.weight(1f)) {
            LazyColumn(
                modifier = Modifier.fillMaxSize().nestedScroll(followWhenAtEnd),
                state = listState,
                contentPadding = PaddingValues(
                    start = contentPadding.calculateStartPadding(layoutDirection),
                    end = contentPadding.calculateEndPadding(layoutDirection),
                    bottom = contentPadding.calculateBottomPadding(),
                ),
            ) {
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
                items(log.entries, key = { it.seq }) { ConnectionRow(it.event) }
            }

            LatestButton(
                visible = !following,
                onClick = {
                    following = true
                    scope.launch { listState.animateScrollToItem(lastIndex()) }
                },
                modifier = Modifier
                    .align(Alignment.BottomCenter)
                    .padding(bottom = contentPadding.calculateBottomPadding() + 16.dp),
            )
        }
    }
}

/** Outside the column, which would otherwise pick its own `AnimatedVisibility`. */
@Composable
private fun LatestButton(visible: Boolean, onClick: () -> Unit, modifier: Modifier) {
    AnimatedVisibility(visible = visible, enter = fadeIn(), exit = fadeOut(), modifier = modifier) {
        ExtendedFloatingActionButton(
            onClick = onClick,
            icon = { Icon(Icons.Default.ArrowDownward, contentDescription = null) },
            text = { Text("Latest") },
        )
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

private val POLL_INTERVAL = 500.milliseconds

/** How close to the oldest entry the list gets before more are loaded. */
private const val LOAD_OLDER_WITHIN = 20
