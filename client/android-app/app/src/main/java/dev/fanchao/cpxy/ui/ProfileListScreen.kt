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
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.MoreVert
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
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
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.ConfigRepository
import dev.fanchao.cpxy.Profile
import dev.fanchao.cpxy.ProfileInstanceManager
import dev.fanchao.cpxy.ui.theme.CpxyTheme
import kotlinx.serialization.Serializable
import java.util.UUID

@Serializable
data object ProfileListRoute

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProfileListScreen(
    configurationRepository: ConfigRepository,
    profileInstanceManager: ProfileInstanceManager,
    navigateToEditScreen: (Profile) -> Unit,
    navigateToNewConfigScreen: () -> Unit,
    navigateToSettingScreen: () -> Unit,
) {
    val showingErrorDialog = remember { mutableStateOf<Throwable?>(null) }

    val configurations by configurationRepository
        .clientConfig
        .collectAsState()

    val runningState by profileInstanceManager
        .state
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

                    IconButton(onClick = navigateToSettingScreen) {
                        Icon(Icons.Default.Settings, contentDescription = "Settings")
                    }
                })
        },
    ) { paddings ->
        ProfileList(
            modifier = Modifier.padding(paddings),
            profiles = configurations.profiles,
            runningState = runningState,
            onItemClick = { configurationRepository.setProfileEnabled(it.id) },
            onEditClick = navigateToEditScreen,
            onDeleteClick = { configurationRepository.deleteProfile(it.id) },
            onErrorInfoClicked = { _, err ->
                showingErrorDialog.value = err
            },
            cloneProfile = {
                val newConfig =
                    it.copy(id = UUID.randomUUID().toString(), name = "${it.name} (Copy)")
                configurationRepository.saveProfile(newConfig)
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
private fun ProfileList(
    modifier: Modifier = Modifier,
    profiles: List<Profile>,
    runningState: ProfileInstanceManager.RunningState,
    onItemClick: (Profile) -> Unit,
    onEditClick: (Profile) -> Unit,
    onDeleteClick: (Profile) -> Unit,
    onErrorInfoClicked: (Profile, Throwable) -> Unit,
    cloneProfile: (Profile) -> Unit,
) {
    var showingDeleteConfirmation by remember { mutableStateOf<Profile?>(null) }

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
        items(profiles) { profile ->
            val showingDropdownMenu = remember { mutableStateOf(false) }
            val isEnabled = runningState.configUsed?.enabledProfileId == profile.id
            val hasError = isEnabled && runningState.startedResult?.isFailure == true

            Row(
                modifier = Modifier
                    .clickable { onItemClick(profile) }
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
                        val title = if (isEnabled && !hasError) "${profile.name} (Running)"
                        else profile.name

                        val style = if (isEnabled) MaterialTheme.typography.titleMedium
                        else MaterialTheme.typography.titleMedium.copy(
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                            fontStyle = FontStyle.Italic
                        )

                        Text(title, style = style)
                    }

                    val text = buildString {
                        append("Global")
                        if (!profile.aiServerUrl.isNullOrBlank()) {
                            append(" - AI")
                        }
                        if (!profile.tailscaleServerUrl.isNullOrBlank()) {
                            append(" - Tailscale")
                        }
                    }

                    Text(
                        text = text,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant
                    )
                }

                if (hasError) {
                    IconButton(onClick = {
                        runningState.startedResult.exceptionOrNull()
                            ?.let { onErrorInfoClicked(profile, it) }
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
                                onEditClick(profile)
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
                                showingDeleteConfirmation = profile
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
                                cloneProfile(profile)
                            },
                        )

                    }
                }
            }
        }
    }

    if (profiles.isEmpty()) {
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
        Profile(
            id = "1",
            name = "Server 1",
            mainServerUrl = "https://main.server1.com",
            aiServerUrl = "https://ai.server1.com",
            tailscaleServerUrl = null
        ),
        Profile(
            id = "2",
            name = "Server 2",
            mainServerUrl = "https://main.server2.com",
            aiServerUrl = null,
            tailscaleServerUrl = null,
        )
    )

    CpxyTheme {
        Surface {
            ProfileList(
                profiles = configurations,
                runningState = ProfileInstanceManager.RunningState(),
                onEditClick = {},
                onItemClick = {},
                onDeleteClick = {},
                onErrorInfoClicked = { _, _ -> },
                cloneProfile = {},
            )
        }

    }
}