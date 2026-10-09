package dev.fanchao.cpxy.vpn.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
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
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.vpn.Profile
import dev.fanchao.cpxy.vpn.ProfileField
import dev.fanchao.cpxy.vpn.ProfileRepository
import dev.fanchao.cpxy.vpn.parseServerList
import dev.fanchao.cpxy.vpn.validationErrors
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlin.uuid.ExperimentalUuidApi
import kotlin.uuid.Uuid

/** A new profile starts with public CN resolvers as its upstream DNS. */
private val DefaultDnsUpstream = listOf("223.5.5.5", "119.29.29.29")

@OptIn(ExperimentalUuidApi::class)
@Composable
fun EditProfileScreen(
    profileId: String?,
    repository: ProfileRepository,
    onDone: () -> Unit,
) {
    // Null while loading.
    val initial by produceState<Profile?>(null, profileId) {
        value = profileId?.let { id -> repository.profiles.first().profiles.firstOrNull { it.id == id } }
            ?: Profile(
                id = Uuid.random().toString(),
                name = "",
                server = "",
                dnsUpstream = DefaultDnsUpstream,
                dnsAlternative = emptyList(),
            )
    }
    initial?.let { profile ->
        EditProfileForm(
            initial = profile,
            isNew = profileId == null,
            repository = repository,
            onDone = onDone,
        )
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun EditProfileForm(
    initial: Profile,
    isNew: Boolean,
    repository: ProfileRepository,
    onDone: () -> Unit,
) {
    var name by remember { mutableStateOf(initial.name) }
    var server by remember { mutableStateOf(initial.server) }
    var dnsUpstream by remember { mutableStateOf(initial.dnsUpstream.joinToString("\n")) }
    var dnsAlternative by remember { mutableStateOf(initial.dnsAlternative.joinToString("\n")) }
    var showErrors by remember { mutableStateOf(false) }
    val scope = rememberCoroutineScope()

    val profile = initial.copy(
        name = name.trim(),
        server = server.trim(),
        dnsUpstream = parseServerList(dnsUpstream),
        dnsAlternative = parseServerList(dnsAlternative),
    )
    val errors = if (showErrors) profile.validationErrors() else emptyMap()

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(if (isNew) "New profile" else "Edit profile") },
                navigationIcon = {
                    IconButton(onClick = onDone) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    if (!isNew) {
                        IconButton(onClick = {
                            scope.launch {
                                repository.delete(initial.id)
                                onDone()
                            }
                        }) {
                            Icon(Icons.Default.Delete, contentDescription = "Delete profile")
                        }
                    }
                },
            )
        },
    ) { padding ->
        Column(
            modifier = Modifier
                .fillMaxSize()
                .padding(padding)
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Field("Name", name, { name = it }, errors[ProfileField.Name])
            Field(
                label = "Server",
                value = server,
                onValueChange = { server = it },
                error = errors[ProfileField.Server],
                hint = "https://:key@example.com",
                keyboardType = KeyboardType.Uri,
            )
            Field(
                label = "Upstream DNS",
                value = dnsUpstream,
                onValueChange = { dnsUpstream = it },
                error = errors[ProfileField.DnsUpstream],
                hint = "One per line. Used when all its answers are in CN.",
                singleLine = false,
                keyboardType = KeyboardType.Uri,
            )
            Field(
                label = "Alternative DNS",
                value = dnsAlternative,
                onValueChange = { dnsAlternative = it },
                error = errors[ProfileField.DnsAlternative],
                hint = "One per line, e.g. https://dns.google/dns-query?ip=8.8.8.8. Used otherwise.",
                singleLine = false,
                keyboardType = KeyboardType.Uri,
            )
            Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                Button(onClick = {
                    showErrors = true
                    if (profile.validationErrors().isEmpty()) {
                        scope.launch {
                            repository.save(profile)
                            onDone()
                        }
                    }
                }) {
                    Text("Save")
                }
            }
        }
    }
}

@Composable
private fun Field(
    label: String,
    value: String,
    onValueChange: (String) -> Unit,
    error: String?,
    hint: String? = null,
    singleLine: Boolean = true,
    keyboardType: KeyboardType = KeyboardType.Text,
) {
    OutlinedTextField(
        value = value,
        onValueChange = onValueChange,
        modifier = Modifier.fillMaxWidth(),
        label = { Text(label) },
        isError = error != null,
        supportingText = (error ?: hint)?.let { { Text(it) } },
        singleLine = singleLine,
        minLines = if (singleLine) 1 else 2,
        keyboardOptions = KeyboardOptions(keyboardType = keyboardType),
    )
}
