package dev.fanchao.cpxy.vpn.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.VpnKey
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.vpn.Profile
import dev.fanchao.cpxy.vpn.ProfileRepository
import dev.fanchao.cpxy.vpn.StoredProfiles
import dev.fanchao.cpxy.vpn.Traffic
import dev.fanchao.cpxy.vpn.VpnController
import dev.fanchao.cpxy.vpn.VpnState
import kotlinx.coroutines.launch

private enum class Tab(val label: String, val title: String, val icon: ImageVector) {
    Vpn("VPN", "CPXY VPN", Icons.Default.VpnKey),
    Traffic("Traffic", "Traffic", Icons.AutoMirrored.Default.List),
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(
    repository: ProfileRepository,
    controller: VpnController,
    onEditProfile: (profileId: String?) -> Unit,
) {
    var tab by rememberSaveable { mutableStateOf(Tab.Vpn) }

    Scaffold(
        topBar = { TopAppBar(title = { Text(tab.title) }) },
        bottomBar = {
            NavigationBar {
                for (item in Tab.entries) {
                    NavigationBarItem(
                        selected = tab == item,
                        onClick = { tab = item },
                        icon = { Icon(item.icon, contentDescription = null) },
                        label = { Text(item.label) },
                    )
                }
            }
        },
        floatingActionButton = {
            if (tab == Tab.Vpn) {
                FloatingActionButton(onClick = { onEditProfile(null) }) {
                    Icon(Icons.Default.Add, contentDescription = "Add profile")
                }
            }
        },
    ) { padding ->
        when (tab) {
            Tab.Vpn -> VpnTab(repository, controller, onEditProfile, padding)
            Tab.Traffic -> TrafficScreen(controller, padding)
        }
    }
}

@Composable
private fun VpnTab(
    repository: ProfileRepository,
    controller: VpnController,
    onEditProfile: (profileId: String?) -> Unit,
    padding: PaddingValues,
) {
    val stored by repository.profiles.collectAsState(StoredProfiles())
    val state by controller.state.collectAsState()
    val traffic by controller.traffic.collectAsState()
    val scope = rememberCoroutineScope()

    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(
            top = padding.calculateTopPadding() + 8.dp,
            bottom = padding.calculateBottomPadding() + 88.dp,
        ),
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
            ListItem(
                modifier = Modifier.clickable { scope.launch { repository.select(profile.id) } },
                leadingContent = {
                    RadioButton(
                        selected = profile.id == stored.selectedId,
                        onClick = { scope.launch { repository.select(profile.id) } },
                    )
                },
                headlineContent = { Text(profile.name) },
                trailingContent = {
                    IconButton(onClick = { onEditProfile(profile.id) }) {
                        Icon(Icons.Default.Edit, contentDescription = "Edit ${profile.name}")
                    }
                },
            )
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
                is VpnState.Connecting -> "Connecting to ${nameOf(state.profileId)}…"
                is VpnState.Connected -> "Connected to ${nameOf(state.profileId)}"
                is VpnState.Failed -> "Failed: ${state.message}"
            }
            Text(
                status,
                style = MaterialTheme.typography.titleMedium,
                color = if (state is VpnState.Failed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.onSurface,
            )
            if (state is VpnState.Connected) {
                Text(
                    "↑ ${formatBytes(traffic.sent)}   ↓ ${formatBytes(traffic.received)}",
                    style = MaterialTheme.typography.bodyMedium,
                )
            }
            Row(verticalAlignment = Alignment.CenterVertically) {
                when (state) {
                    is VpnState.Connecting, is VpnState.Connected ->
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

@Composable
private fun SectionTitle(text: String) {
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
