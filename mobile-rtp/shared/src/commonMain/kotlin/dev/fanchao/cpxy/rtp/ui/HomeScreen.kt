package dev.fanchao.cpxy.rtp.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.selectable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.rtp.AppFilter
import dev.fanchao.cpxy.rtp.AppSettings
import dev.fanchao.cpxy.rtp.Platform
import dev.fanchao.cpxy.rtp.Profile
import dev.fanchao.cpxy.rtp.ProfileRepository
import dev.fanchao.cpxy.rtp.SettingsRepository
import dev.fanchao.cpxy.rtp.StoredProfiles
import dev.fanchao.cpxy.rtp.Traffic
import dev.fanchao.cpxy.rtp.TunnelState
import dev.fanchao.cpxy.rtp.VpnController
import dev.fanchao.cpxy.rtp.VpnState
import kotlinx.coroutines.launch

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(
    repository: ProfileRepository,
    settingsRepository: SettingsRepository,
    controller: VpnController,
    platform: Platform,
    onEditProfile: (profileId: String?) -> Unit,
    onChooseApps: (AppFilter) -> Unit,
) {
    val stored by repository.profiles.collectAsState(StoredProfiles())
    val settings by settingsRepository.settings.collectAsState(AppSettings())
    val state by controller.state.collectAsState()
    val traffic by controller.traffic.collectAsState()
    val scope = rememberCoroutineScope()

    Scaffold(
        topBar = { TopAppBar(title = { Text("CPXY RTP") }) },
        floatingActionButton = {
            FloatingActionButton(onClick = { onEditProfile(null) }) {
                Icon(Icons.Default.Add, contentDescription = "Add profile")
            }
        },
    ) { padding ->
        LazyColumn(
            modifier = Modifier.fillMaxSize().padding(padding),
            contentPadding = PaddingValues(top = 8.dp, bottom = 88.dp),
        ) {
            item {
                StatusCard(
                    state = state,
                    traffic = traffic,
                    stored = stored,
                    onConnect = { profile -> scope.launch { controller.connect(profile) } },
                    onDisconnect = controller::disconnect,
                )
            }

            item { SectionTitle("Profiles") }
            if (stored.profiles.isEmpty()) {
                item {
                    Text(
                        "Add a profile with the + button.",
                        modifier = Modifier.padding(horizontal = 16.dp),
                        style = MaterialTheme.typography.bodyMedium,
                    )
                }
            }
            items(stored.profiles, key = { it.id }) { profile ->
                // Selection and editing are sibling focus targets, so a D-pad reaches both.
                Row(verticalAlignment = Alignment.CenterVertically) {
                    ListItem(
                        modifier = Modifier.weight(1f).selectable(
                            selected = profile.id == stored.selectedId,
                            role = Role.RadioButton,
                            onClick = { scope.launch { repository.select(profile.id) } },
                        ),
                        leadingContent = {
                            RadioButton(selected = profile.id == stored.selectedId, onClick = null)
                        },
                        headlineContent = { Text(profile.name) },
                        supportingContent = { Text(profile.endpoint) },
                    )
                    IconButton(onClick = { onEditProfile(profile.id) }) {
                        Icon(Icons.Default.Edit, contentDescription = "Edit ${profile.name}")
                    }
                }
            }

            platform.apps?.let {
                item { SectionTitle("Apps using the VPN") }
                items(AppFilter.entries) { filter ->
                    val count = settings.appsFor(filter).size
                    Row(verticalAlignment = Alignment.CenterVertically) {
                        ListItem(
                            modifier = Modifier.weight(1f).selectable(
                                selected = settings.appFilter == filter,
                                role = Role.RadioButton,
                                onClick = {
                                    scope.launch { settingsRepository.update { it.copy(appFilter = filter) } }
                                },
                            ),
                            leadingContent = { RadioButton(selected = settings.appFilter == filter, onClick = null) },
                            headlineContent = { Text(filter.title) },
                            supportingContent = filter.description(count)?.let { { Text(it) } },
                        )
                        if (filter != AppFilter.AllApps) {
                            TextButton(onClick = { onChooseApps(filter) }) { Text("Choose") }
                        }
                    }
                }
                if (state !is VpnState.Disconnected && state !is VpnState.Failed) {
                    item {
                        Text(
                            "Changes apply the next time the VPN connects.",
                            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }

            item {
                Text(
                    "Version ${platform.versionName}",
                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 24.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@Composable
private fun StatusCard(
    state: VpnState,
    traffic: Traffic,
    stored: StoredProfiles,
    onConnect: (Profile) -> Unit,
    onDisconnect: () -> Unit,
) {
    fun nameOf(id: String) = stored.profiles.firstOrNull { it.id == id }?.name ?: "a deleted profile"

    Card(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp)) {
        Column(
            modifier = Modifier.padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            val status = when (state) {
                VpnState.Disconnected -> "Disconnected"
                is VpnState.Starting -> "Starting ${nameOf(state.profileId)}…"
                is VpnState.Running -> when (state.tunnel) {
                    TunnelState.Connecting -> "Connecting to ${nameOf(state.profileId)}…"
                    TunnelState.Connected -> "Connected to ${nameOf(state.profileId)}"
                    TunnelState.Repairing -> "Reconnecting to ${nameOf(state.profileId)}…"
                    TunnelState.Degraded -> "${nameOf(state.profileId)} is not answering"
                    TunnelState.Restarting -> "Restarting ${nameOf(state.profileId)}…"
                }
                is VpnState.Failed -> "Failed: ${state.message}"
            }
            Text(
                status,
                style = MaterialTheme.typography.titleMedium,
                color = if (state is VpnState.Failed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface,
            )
            if (state is VpnState.Running) {
                Text(
                    "↑ ${formatBytes(traffic.sent)}   ↓ ${formatBytes(traffic.received)}",
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                when (state) {
                    is VpnState.Starting, is VpnState.Running ->
                        OutlinedButton(onClick = onDisconnect) { Text("Disconnect") }

                    else -> {
                        val selected = stored.selected
                        Button(onClick = { selected?.let(onConnect) }, enabled = selected != null) {
                            Text(if (selected != null) "Connect to ${selected.name}" else "Connect")
                        }
                    }
                }
            }
        }
    }
}

private val AppFilter.title: String
    get() = when (this) {
        AppFilter.AllApps -> "All apps"
        AppFilter.OnlySelected -> "Only selected apps"
        AppFilter.ExceptSelected -> "All apps except selected"
    }

private fun AppFilter.description(count: Int): String? = when (this) {
    AppFilter.AllApps -> null
    AppFilter.OnlySelected ->
        when (count) {
            0 -> "No apps selected, so all apps use the VPN"
            1 -> "1 app uses the VPN"
            else -> "$count apps use the VPN"
        }
    AppFilter.ExceptSelected ->
        when (count) {
            0 -> "No apps selected"
            1 -> "1 app bypasses the VPN"
            else -> "$count apps bypass the VPN"
        }
}

@Composable
internal fun SectionTitle(text: String) {
    Text(
        text,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 24.dp, bottom = 8.dp),
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
    )
}

internal fun formatBytes(bytes: Long): String {
    val units = listOf("B", "KB", "MB", "GB", "TB")
    var value = bytes.toDouble()
    var unit = 0
    while (value >= 1024 && unit < units.lastIndex) {
        value /= 1024
        unit++
    }
    if (unit == 0) return "$bytes B"
    val tenths = (value * 10).toLong()
    return "${tenths / 10}.${tenths % 10} ${units[unit]}"
}
