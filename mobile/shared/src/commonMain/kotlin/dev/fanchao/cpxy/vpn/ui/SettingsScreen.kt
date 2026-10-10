package dev.fanchao.cpxy.vpn.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.selection.selectable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.vpn.AppFilter
import dev.fanchao.cpxy.vpn.AppSettings
import dev.fanchao.cpxy.vpn.Platform
import dev.fanchao.cpxy.vpn.SettingsRepository
import dev.fanchao.cpxy.vpn.VpnController
import dev.fanchao.cpxy.vpn.VpnState
import kotlinx.coroutines.launch

@Composable
internal fun SettingsScreen(
    settingsRepository: SettingsRepository,
    controller: VpnController,
    platform: Platform,
    onChooseApps: (AppFilter) -> Unit,
    contentPadding: PaddingValues,
) {
    val settings by settingsRepository.settings.collectAsState(AppSettings())
    val state by controller.state.collectAsState()
    val scope = rememberCoroutineScope()

    LazyColumn(
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(
            top = contentPadding.calculateTopPadding(),
            bottom = contentPadding.calculateBottomPadding() + 16.dp,
        ),
    ) {
        if (platform.openAlwaysOnSettings != null || platform.addQuickSettingsTile != null) {
            item { SectionTitle("Connection") }
        }
        platform.openAlwaysOnSettings?.let { open ->
            item {
                ListItem(
                    modifier = Modifier.clickable(onClick = open),
                    headlineContent = { Text("Always-on VPN") },
                    supportingContent = {
                        Text(
                            "Connect with the selected profile when the device starts, and keep " +
                                "the VPN on. Turn it on for CPXY VPN in the system's VPN settings.",
                        )
                    },
                    trailingContent = { Icon(Icons.AutoMirrored.Default.KeyboardArrowRight, contentDescription = null) },
                )
            }
        }
        platform.addQuickSettingsTile?.let { add ->
            item {
                ListItem(
                    modifier = Modifier.clickable(onClick = add),
                    headlineContent = { Text("Quick settings tile") },
                    supportingContent = { Text("Connect and disconnect from the quick settings panel.") },
                    trailingContent = { Icon(Icons.AutoMirrored.Default.KeyboardArrowRight, contentDescription = null) },
                )
            }
        }

        if (platform.apps != null) {
            item { SectionTitle("Apps using the VPN") }
            for (filter in AppFilter.entries) {
                item(key = filter) {
                    val count = settings.appsFor(filter).size
                    ListItem(
                        modifier = Modifier.selectable(
                            selected = settings.appFilter == filter,
                            role = Role.RadioButton,
                            onClick = { scope.launch { settingsRepository.update { it.copy(appFilter = filter) } } },
                        ),
                        leadingContent = { RadioButton(selected = settings.appFilter == filter, onClick = null) },
                        headlineContent = { Text(filter.title) },
                        supportingContent = filter.description(count)?.let { { Text(it) } },
                        trailingContent = if (filter == AppFilter.AllApps) {
                            null
                        } else {
                            {
                                Text(
                                    "Choose",
                                    modifier = Modifier.clickable { onChooseApps(filter) }.padding(8.dp),
                                    color = MaterialTheme.colorScheme.primary,
                                    style = MaterialTheme.typography.labelLarge,
                                )
                            }
                        },
                    )
                }
            }
            if (state is VpnState.Connecting || state is VpnState.Connected) {
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
