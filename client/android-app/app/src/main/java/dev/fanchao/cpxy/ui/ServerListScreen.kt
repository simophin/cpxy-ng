package dev.fanchao.cpxy.ui

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Create
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Badge
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.ClientConfiguration
import dev.fanchao.cpxy.ClientConfigurationRepository
import dev.fanchao.cpxy.ClientInstanceManager
import dev.fanchao.cpxy.R
import dev.fanchao.cpxy.ui.theme.CpxyTheme
import kotlinx.serialization.Serializable
import java.util.UUID

@Serializable
data object ServerListRoute

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ServerListScreen(
    configurationRepository: ClientConfigurationRepository,
    clientInstanceManager: ClientInstanceManager,
    navigateToEditScreen: (ClientConfiguration) -> Unit,
    navigateToNewConfigScreen: () -> Unit,
) {
    val showingErrorDialog = remember { mutableStateOf<Throwable?>(null) }

    val configurations by configurationRepository
        .configurations
        .collectAsState()

    val instanceState by clientInstanceManager
        .state
        .collectAsState()

    val serviceStarted by clientInstanceManager
        .started
        .collectAsState()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Servers") },
                actions = {
                    IconButton(onClick = navigateToNewConfigScreen) {
                        Icon(
                            Icons.Default.Add,
                            contentDescription = "Add"
                        )
                    }
                })
        },
        floatingActionButton = {
            FloatingActionButton(onClick = {
                if (serviceStarted) {
                    clientInstanceManager.stop()
                } else {
                    clientInstanceManager.start()
                }
            }) {
                if (serviceStarted) {
                    Icon(
                        painterResource(R.drawable.baseline_stop_24),
                        contentDescription = "stop"
                    )
                } else {
                    Icon(Icons.Default.PlayArrow, contentDescription = "Start")
                }
            }
        }
    ) { paddings ->
        ServerList(
            modifier = Modifier.padding(paddings),
            configurations = configurations,
            instanceState = instanceState,
            onItemClick = navigateToEditScreen,
            onEditClick = navigateToEditScreen,
            onDeleteClick = { configurationRepository.delete(it.id) },
            onErrorInfoClicked = { _, err ->
                showingErrorDialog.value = err
            },
            toggleConfig = { configurationRepository.setConfigEnabled(it.id, !it.enabled) },
            cloneConfig = {
                val newConfig =
                    it.copy(id = UUID.randomUUID().toString(), name = "${it.name} (Copy)")
                configurationRepository.save(newConfig)
                navigateToEditScreen(newConfig)
            }
        )

        if (showingErrorDialog.value != null) {
            AlertDialog(
                onDismissRequest = { showingErrorDialog.value = null },
                confirmButton = {
                    OutlinedButton(onClick = { showingErrorDialog.value = null }) {
                        Text("OK")
                    }
                },
                text = {
                    Text(showingErrorDialog.value!!.message.orEmpty())
                }
            )
        }
    }
}

@Composable
private fun ServerList(
    modifier: Modifier = Modifier,
    configurations: List<ClientConfiguration>,
    instanceState: Map<String, ClientInstanceManager.InstanceState>,
    onItemClick: (ClientConfiguration) -> Unit,
    onEditClick: (ClientConfiguration) -> Unit,
    onDeleteClick: (ClientConfiguration) -> Unit,
    onErrorInfoClicked: (ClientConfiguration, Throwable) -> Unit,
    toggleConfig: (ClientConfiguration) -> Unit,
    cloneConfig: (ClientConfiguration) -> Unit,
) {
    var showingDeleteConfirmation by remember { mutableStateOf<ClientConfiguration?>(null) }

    if (showingDeleteConfirmation != null) {
        AlertDialog(
            onDismissRequest = { showingDeleteConfirmation = null },
            confirmButton = {
                OutlinedButton(onClick = {
                    onDeleteClick(showingDeleteConfirmation!!)
                    showingDeleteConfirmation = null
                }) {
                    Text("Delete")
                }
            },
            dismissButton = {
                TextButton(onClick = { showingDeleteConfirmation = null }) {
                    Text("Cancel")
                }
            },
            text = { Text("Delete ${showingDeleteConfirmation!!.name}?") }
        )
    }

    LazyColumn(modifier = modifier.fillMaxSize()) {
        items(configurations) { config ->
            val showingDropdownMenu = remember { mutableStateOf(false) }

            Row(
                modifier = Modifier
                    .clickable { onItemClick(config) }
                    .padding(8.dp),
                verticalAlignment = Alignment.CenterVertically) {
                Column(
                    modifier = Modifier
                        .weight(1f)
                        .padding(horizontal = 8.dp),
                    verticalArrangement = Arrangement.spacedBy(4.dp)
                ) {
                    Row(
                        horizontalArrangement = Arrangement.spacedBy(4.dp),
                        verticalAlignment = Alignment.CenterVertically
                    ) {
                        val title = if (config.enabled) config.name
                        else "${config.name} (Disabled)"

                        val style = if (config.enabled) MaterialTheme.typography.titleMedium
                        else MaterialTheme.typography.titleMedium.copy(
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            fontStyle = FontStyle.Italic
                        )

                        Text(title, style = style)

                        if (config.enabled) {
                            Badge(containerColor = MaterialTheme.colorScheme.secondaryContainer) {
                                Text(config.bindAddress)
                            }
                        }
                    }

                    Text(
                        text = "${config.serverHost}:${config.serverPort}",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                }

                val state = instanceState[config.id]

                if (state?.instance?.isFailure == true) {
                    IconButton(onClick = {
                        state.instance.exceptionOrNull()?.let { onErrorInfoClicked(config, it) }
                    }) {
                        Icon(
                            Icons.Default.Warning,
                            modifier = Modifier.size(20.dp),
                            tint = MaterialTheme.colorScheme.error,
                            contentDescription = "Error"
                        )
                    }
                }

                IconButton(onClick = { showingDropdownMenu.value = true }) {
                    Icon(
                        Icons.Default.MoreVert,
                        modifier = Modifier.size(20.dp),
                        contentDescription = "More"
                    )

                    DropdownMenu(
                        expanded = showingDropdownMenu.value,
                        onDismissRequest = { showingDropdownMenu.value = false },
                    ) {
                        DropdownMenuItem(
                            text = { Text("Edit") },
                            onClick = {
                                showingDropdownMenu.value = false
                                onEditClick(config)
                            },
                            leadingIcon = {
                                Icon(
                                    Icons.Default.Edit,
                                    contentDescription = null
                                )
                            }
                        )

                        DropdownMenuItem(
                            text = { Text("Delete") },
                            onClick = {
                                showingDropdownMenu.value = false
                                showingDeleteConfirmation = config
                            },
                            leadingIcon = {
                                Icon(
                                    Icons.Default.Delete,
                                    contentDescription = null
                                )
                            }
                        )

                        DropdownMenuItem(
                            text = { Text("Clone") },
                            onClick = {
                                showingDropdownMenu.value = false
                                cloneConfig(config)
                            },
                        )

                        DropdownMenuItem(
                            text = { Text(if (config.enabled) "Disable" else "Enable") },
                            onClick = {
                                showingDropdownMenu.value = false
                                toggleConfig(config)
                            },
                        )
                    }
                }
            }
        }
    }

    if (configurations.isEmpty()) {
        Box(modifier = modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            Text(
                "No configurations yet",
                style = MaterialTheme.typography.bodyLarge
            )
        }
    }
}

@Composable
@Preview
private fun ServerListPreview() {
    val configurations = listOf(
        ClientConfiguration(
            id = "1",
            name = "Server 1",
            serverHost = "myhost",
            serverPort = 80.toUShort(),
            key = "xxx",
            bindAddress = "127.0.0.1:8080",
            enabled = false,
        )
    )

    CpxyTheme {
        ServerList(
            configurations = configurations,
            instanceState = mapOf(
                "1" to ClientInstanceManager.InstanceState(
                    instance = Result.failure(RuntimeException("Error")),
                    config = configurations[0]
                )
            ),
            onEditClick = {},
            onItemClick = {},
            onDeleteClick = {},
            onErrorInfoClicked = { _, _ -> },
            toggleConfig = {},
            cloneConfig = {},
        )
    }
}