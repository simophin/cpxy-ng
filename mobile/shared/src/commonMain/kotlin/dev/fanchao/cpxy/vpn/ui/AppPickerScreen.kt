package dev.fanchao.cpxy.vpn.ui

import androidx.compose.foundation.Image
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.selection.toggleable
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Checkbox
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.vpn.AppCatalog
import dev.fanchao.cpxy.vpn.AppFilter
import dev.fanchao.cpxy.vpn.InstalledApp
import dev.fanchao.cpxy.vpn.SettingsRepository
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/** Chooses the apps of [filter]. Each tap is saved at once. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun AppPickerScreen(
    filter: AppFilter,
    settingsRepository: SettingsRepository,
    catalog: AppCatalog,
    onDone: () -> Unit,
) {
    // Selected apps are listed first, as they were when the screen opened, so rows do not move
    // while the user ticks them. Null while loading.
    val loaded by produceState<LoadedApps?>(null, filter) {
        val selected = settingsRepository.settings.first().appsFor(filter)
        value = LoadedApps(
            apps = catalog.apps().sortedWith(
                compareBy<InstalledApp> { it.packageName !in selected }.thenBy { it.label.lowercase() },
            ),
            initiallySelected = selected,
        )
    }
    val selected by produceState<Set<String>>(emptySet(), filter) {
        settingsRepository.settings.collect { value = it.appsFor(filter) }
    }
    var query by rememberSaveable { mutableStateOf("") }
    var showSystem by rememberSaveable { mutableStateOf(false) }
    var menuOpen by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()

    Scaffold(
        topBar = {
            TopAppBar(
                title = {
                    Text(if (filter == AppFilter.OnlySelected) "Apps using the VPN" else "Apps bypassing the VPN")
                },
                navigationIcon = {
                    IconButton(onClick = onDone) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    IconButton(onClick = { menuOpen = true }) {
                        Icon(Icons.Default.MoreVert, contentDescription = "More options")
                    }
                    DropdownMenu(expanded = menuOpen, onDismissRequest = { menuOpen = false }) {
                        DropdownMenuItem(
                            text = { Text("Show system apps") },
                            leadingIcon = { Checkbox(checked = showSystem, onCheckedChange = null) },
                            onClick = {
                                showSystem = !showSystem
                                menuOpen = false
                            },
                        )
                        DropdownMenuItem(
                            text = { Text("Clear selection") },
                            enabled = selected.isNotEmpty(),
                            onClick = {
                                scope.launch { settingsRepository.update { it.withApps(filter, emptySet()) } }
                                menuOpen = false
                            },
                        )
                    }
                },
            )
        },
    ) { padding ->
        val apps = loaded
        if (apps == null) {
            Box(Modifier.fillMaxSize().padding(padding), contentAlignment = Alignment.Center) {
                CircularProgressIndicator()
            }
            return@Scaffold
        }

        val words = query.trim().lowercase()
        val shown = apps.apps.filter { app ->
            (showSystem || !app.isSystem || app.packageName in apps.initiallySelected) &&
                (words.isEmpty() || words in app.label.lowercase() || words in app.packageName.lowercase())
        }
        LazyColumn(
            modifier = Modifier.fillMaxSize(),
            contentPadding = PaddingValues(
                top = padding.calculateTopPadding(),
                bottom = padding.calculateBottomPadding() + 16.dp,
            ),
        ) {
            item {
                OutlinedTextField(
                    value = query,
                    onValueChange = { query = it },
                    modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
                    placeholder = { Text("Search apps") },
                    leadingIcon = { Icon(Icons.Default.Search, contentDescription = null) },
                    singleLine = true,
                )
            }
            items(shown, key = { it.packageName }) { app ->
                val checked = app.packageName in selected
                ListItem(
                    modifier = Modifier.toggleable(
                        value = checked,
                        role = Role.Checkbox,
                        onValueChange = { add ->
                            scope.launch {
                                settingsRepository.update {
                                    val apps = it.appsFor(filter)
                                    it.withApps(filter, if (add) apps + app.packageName else apps - app.packageName)
                                }
                            }
                        },
                    ),
                    leadingContent = { AppIcon(catalog, app.packageName) },
                    headlineContent = { Text(app.label, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    supportingContent = {
                        Text(app.packageName, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    },
                    trailingContent = { Checkbox(checked = checked, onCheckedChange = null) },
                )
            }
        }
    }
}

private class LoadedApps(val apps: List<InstalledApp>, val initiallySelected: Set<String>)

@Composable
private fun AppIcon(catalog: AppCatalog, packageName: String) {
    val icon by produceState<ImageBitmap?>(null, packageName) { value = catalog.icon(packageName) }
    Box(Modifier.size(40.dp)) {
        icon?.let { Image(it, contentDescription = null, modifier = Modifier.fillMaxSize()) }
    }
}
