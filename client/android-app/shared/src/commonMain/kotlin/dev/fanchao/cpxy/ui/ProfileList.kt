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
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material.icons.filled.Edit
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Warning
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.app.ConfigLoadState
import dev.fanchao.cpxy.app.ConfigRepository
import dev.fanchao.cpxy.app.Profile
import dev.fanchao.cpxy.app.ProfileInstanceManager
import dev.fanchao.cpxy.app.ProfileInstanceManager.RunningState
import dev.fanchao.cpxy.ui.theme.CpxyTheme
import kotlinx.coroutines.launch
import org.jetbrains.compose.ui.tooling.preview.Preview
import kotlin.uuid.ExperimentalUuidApi
import kotlin.uuid.Uuid


@OptIn(ExperimentalMaterial3Api::class, ExperimentalUuidApi::class)
@Composable
fun ProfileList(
    modifier: Modifier = Modifier,
    navigateToEditScreen: (Profile) -> Unit,
    configurationRepository: ConfigRepository,
    profileInstanceManager: ProfileInstanceManager,
) {
    val showingErrorDialog = remember { mutableStateOf<Throwable?>(null) }
    val loadState by configurationRepository.loadState.collectAsState()
    val scope = rememberCoroutineScope()

    val runningState by profileInstanceManager
        .state
        .collectAsState()

    when (val current = loadState) {
        ConfigLoadState.Loading -> Box(modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            CircularProgressIndicator()
        }
        is ConfigLoadState.Error -> Box(modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            Text("Unable to load configuration: ${current.cause.message.orEmpty()}")
        }
        is ConfigLoadState.Loaded -> ProfileList(
            modifier = modifier,
            profiles = current.config.profiles,
            runningState = runningState,
            onEditClick = navigateToEditScreen,
            onDeleteClick = { profile -> scope.launch { runCatching { configurationRepository.deleteProfile(profile.id) }.onFailure { showingErrorDialog.value = it } } },
            onEnableClick = { profile -> scope.launch { runCatching { configurationRepository.setProfileEnabled(profile.id) }.onFailure { showingErrorDialog.value = it } } },
            onDisableClick = { scope.launch { runCatching { configurationRepository.setProfileEnabled(null) }.onFailure { showingErrorDialog.value = it } } },
            onErrorInfoClicked = { _, err -> showingErrorDialog.value = err },
            cloneProfile = { profile ->
                scope.launch {
                    val clone = profile.copy(id = Uuid.random().toString(), name = "${profile.name} (Copy)")
                    runCatching { configurationRepository.saveProfile(clone) }
                        .onSuccess { navigateToEditScreen(clone) }
                        .onFailure { showingErrorDialog.value = it }
                }
            },
        )
    }

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

@Composable
private fun ProfileList(
    modifier: Modifier = Modifier,
    profiles: List<Profile>,
    runningState: RunningState,
    onEnableClick: (Profile) -> Unit,
    onDisableClick: (Profile) -> Unit,
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
                    .clickable { showingDropdownMenu.value = true }
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

                DropdownMenu(
                    expanded = showingDropdownMenu.value,
                    onDismissRequest = { showingDropdownMenu.value = false },
                ) {
                    if (isEnabled) {
                        DropdownMenuItem(
                            text = { Text("Stop") },
                            onClick = {
                                showingDropdownMenu.value = false
                                onDisableClick(profile)
                            },
                            leadingIcon = {
                                Icon(Icons.Default.Close, null)
                            }
                        )
                    } else {
                        DropdownMenuItem(
                            text = { Text("Enable") },
                            onClick = {
                                showingDropdownMenu.value = false
                                onEnableClick(profile)
                            },
                            leadingIcon = {
                                Icon(Icons.Default.PlayArrow, null)
                            }
                        )
                    }

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
private fun ProfileListPreview() {
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
                runningState = RunningState(),
                onEditClick = {},
                onDeleteClick = {},
                onErrorInfoClicked = { _, _ -> },
                cloneProfile = {},
                onEnableClick = {},
                onDisableClick = {}
            )
        }

    }
}
