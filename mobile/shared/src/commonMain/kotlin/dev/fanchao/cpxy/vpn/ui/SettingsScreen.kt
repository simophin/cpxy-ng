package dev.fanchao.cpxy.vpn.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.KeyboardArrowRight
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.vpn.isIpv4Cidr
import dev.fanchao.cpxy.vpn.parseProxyRoutes
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

        item { SectionTitle("Routing") }
        item {
            ProxyRoutesSetting(settings.proxyRoutes) { routes ->
                scope.launch { settingsRepository.update { it.copy(proxyRoutes = routes) } }
            }
        }

        item { SectionTitle("Share connection") }
        item {
            Text(
                "Use this device's Wi-Fi IP address as the proxy host on other devices. " +
                    "Proxies listen on all IPv4 interfaces while the VPN is connected, without authentication. " +
                    "Changes apply the next time the VPN connects.",
                modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        item {
            ProxySetting(
                name = "HTTP proxy",
                enabled = settings.httpProxyEnabled,
                port = settings.httpProxyPort,
                otherPort = settings.socksProxyPort.takeIf { settings.socksProxyEnabled },
                onEnabled = { enabled -> scope.launch { settingsRepository.update { it.copy(httpProxyEnabled = enabled) } } },
                onPort = { port -> scope.launch { settingsRepository.update { it.copy(httpProxyPort = port) } } },
            )
        }
        item {
            ProxySetting(
                name = "SOCKS5 proxy",
                enabled = settings.socksProxyEnabled,
                port = settings.socksProxyPort,
                otherPort = settings.httpProxyPort.takeIf { settings.httpProxyEnabled },
                onEnabled = { enabled -> scope.launch { settingsRepository.update { it.copy(socksProxyEnabled = enabled) } } },
                onPort = { port -> scope.launch { settingsRepository.update { it.copy(socksProxyPort = port) } } },
            )
        }

        item { SectionTitle("About") }
        item {
            ListItem(
                headlineContent = { Text("Version") },
                supportingContent = { Text(platform.versionName) },
            )
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
private fun ProxySetting(
    name: String,
    enabled: Boolean,
    port: Int,
    otherPort: Int?,
    onEnabled: (Boolean) -> Unit,
    onPort: (Int) -> Unit,
) {
    var editing by remember { mutableStateOf(false) }
    var text by remember(port) { mutableStateOf(port.toString()) }
    val conflicting = otherPort == port
    ListItem(
        headlineContent = { Text(name) },
        supportingContent = { Text(if (!enabled && conflicting) "Choose a different port before enabling" else "Listen on port $port") },
        trailingContent = {
            Switch(checked = enabled, onCheckedChange = onEnabled, enabled = enabled || !conflicting)
        },
    )
    ListItem(
        modifier = Modifier.clickable { text = port.toString(); editing = true },
        headlineContent = { Text("$name port") },
        supportingContent = { Text(port.toString()) },
        trailingContent = { Icon(Icons.AutoMirrored.Default.KeyboardArrowRight, contentDescription = null) },
    )
    if (editing) {
        val parsed = text.toIntOrNull()
        val error = when {
            parsed == null || parsed !in 1..65535 -> "Enter a port between 1 and 65535"
            enabled && parsed == otherPort -> "HTTP and SOCKS ports must be different"
            else -> null
        }
        AlertDialog(
            onDismissRequest = { editing = false },
            title = { Text("$name port") },
            text = {
                OutlinedTextField(
                    value = text,
                    onValueChange = { text = it },
                    label = { Text("Port") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
                    isError = error != null,
                    supportingText = error?.let { { Text(it) } },
                )
            },
            confirmButton = {
                TextButton(enabled = error == null, onClick = { onPort(parsed!!); editing = false }) { Text("Save") }
            },
            dismissButton = { TextButton(onClick = { editing = false }) { Text("Cancel") } },
        )
    }
}

@Composable
private fun ProxyRoutesSetting(routes: List<String>, onSave: (List<String>) -> Unit) {
    var editing by remember { mutableStateOf(false) }
    var text by remember { mutableStateOf("") }
    ListItem(
        modifier = Modifier.clickable { text = routes.joinToString("\n"); editing = true },
        headlineContent = { Text("Proxy routes") },
        supportingContent = {
            Text(if (routes.isEmpty()) "No fixed routes" else "${routes.size} fixed routes")
        },
        trailingContent = { Icon(Icons.AutoMirrored.Default.KeyboardArrowRight, contentDescription = null) },
    )
    if (editing) {
        val parsed = parseProxyRoutes(text)
        val invalid = parsed.firstOrNull { !isIpv4Cidr(it) }
        AlertDialog(
            onDismissRequest = { editing = false },
            title = { Text("Proxy routes") },
            text = {
                OutlinedTextField(
                    value = text,
                    onValueChange = { text = it },
                    label = { Text("IPv4 CIDRs, one per line") },
                    placeholder = { Text("203.0.113.0/24\n223.5.5.5/32") },
                    minLines = 5,
                    maxLines = 10,
                    isError = invalid != null,
                    supportingText = {
                        Text(if (invalid != null) "Invalid IPv4 CIDR: $invalid" else
                            "Matching destination IPs, including resolved names, always use the main proxy. " +
                            "Changes apply the next time the VPN connects. Clear the list to remove fixed routes.")
                    },
                )
            },
            confirmButton = {
                TextButton(enabled = invalid == null, onClick = { onSave(parsed); editing = false }) { Text("Save") }
            },
            dismissButton = { TextButton(onClick = { editing = false }) { Text("Cancel") } },
        )
    }
}
