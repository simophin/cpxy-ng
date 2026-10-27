package dev.fanchao.cpxy.rtp.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
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
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.input.VisualTransformation
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.rtp.KeyVault
import dev.fanchao.cpxy.rtp.MinPasswordLength
import dev.fanchao.cpxy.rtp.Profile
import dev.fanchao.cpxy.rtp.ProfileField
import dev.fanchao.cpxy.rtp.ProfileRepository
import dev.fanchao.cpxy.rtp.parseServerList
import dev.fanchao.cpxy.rtp.validationErrors
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlin.uuid.ExperimentalUuidApi
import kotlin.uuid.Uuid

@OptIn(ExperimentalUuidApi::class)
@Composable
fun EditProfileScreen(
    profileId: String?,
    repository: ProfileRepository,
    keys: KeyVault,
    onDone: () -> Unit,
) {
    // Null while loading.
    val initial by produceState<Profile?>(null, profileId) {
        value = profileId?.let { id -> repository.profiles.first().profiles.firstOrNull { it.id == id } }
            ?: Profile(id = Uuid.random().toString(), name = "", endpoint = "")
    }
    initial?.let { profile ->
        EditProfileForm(profile, isNew = profileId == null, repository, keys, onDone)
    }
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
private fun EditProfileForm(
    initial: Profile,
    isNew: Boolean,
    repository: ProfileRepository,
    keys: KeyVault,
    onDone: () -> Unit,
) {
    var name by remember(initial.id) { mutableStateOf(initial.name) }
    var endpoint by remember(initial.id) { mutableStateOf(initial.endpoint) }
    var serverIdentity by remember(initial.id) { mutableStateOf(initial.serverIdentity) }
    var password by remember(initial.id) { mutableStateOf("") }
    var dnsUpstream by remember(initial.id) { mutableStateOf(initial.dnsUpstream.joinToString("\n")) }
    var dnsAlternative by remember(initial.id) { mutableStateOf(initial.dnsAlternative.joinToString("\n")) }
    var showErrors by remember(initial.id) { mutableStateOf(false) }
    var saving by remember(initial.id) { mutableStateOf(false) }
    var saveError by remember(initial.id) { mutableStateOf<String?>(null) }
    val scope = rememberCoroutineScope()
    val focusManager = LocalFocusManager.current

    val profile = initial.copy(
        name = name.trim(),
        endpoint = endpoint.trim(),
        serverIdentity = serverIdentity,
        dnsUpstream = parseServerList(dnsUpstream),
        dnsAlternative = parseServerList(dnsAlternative),
    )

    // A saved profile keeps its key unless a new password is given.
    fun allErrors(): Map<ProfileField, String> = profile.validationErrors() + buildMap {
        when {
            password.isEmpty() && isNew -> put(ProfileField.Password, "The endpoint's password is required")
            password.isNotEmpty() && password.length < MinPasswordLength ->
                put(ProfileField.Password, "At least $MinPasswordLength characters")
        }
    }
    val errors = if (showErrors) allErrors() else emptyMap()

    fun save() {
        showErrors = true
        if (saving || allErrors().isNotEmpty()) return
        saving = true
        saveError = null
        scope.launch {
            try {
                if (password.isNotEmpty()) keys.store(profile.id, password)
                repository.save(profile)
                onDone()
            } catch (e: Exception) {
                saveError = e.message ?: e.toString()
            } finally {
                saving = false
            }
        }
    }

    val next = { focusManager.moveFocus(FocusDirection.Next); Unit }

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
                                keys.delete(initial.id)
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
                .consumeWindowInsets(padding)
                .imePadding()
                .verticalScroll(rememberScrollState())
                .padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Field("Name", name, { name = it }, errors[ProfileField.Name], onNext = next)
            Field(
                label = "Endpoint",
                value = endpoint,
                onValueChange = { endpoint = it },
                error = errors[ProfileField.Endpoint],
                onNext = next,
                hint = "HTTP control host:port, such as vpn.example.com:30000 or [2001:db8::1]:30000",
                keyboardType = KeyboardType.Uri,
            )
            Field(
                label = "Password",
                value = password,
                onValueChange = { password = it },
                error = errors[ProfileField.Password],
                onNext = next,
                hint = if (isNew) "The endpoint's CPXY_RTP_PASSWORD" else "Leave empty to keep the saved key",
                keyboardType = KeyboardType.Password,
                visualTransformation = PasswordVisualTransformation(),
            )
            Field(
                label = "Server identity",
                value = serverIdentity,
                onValueChange = { serverIdentity = it },
                error = errors[ProfileField.ServerIdentity],
                onNext = next,
                hint = "Must match the endpoint. The endpoint assigns the tunnel address automatically.",
            )
            Field(
                label = "Upstream DNS",
                value = dnsUpstream,
                onValueChange = { dnsUpstream = it },
                error = errors[ProfileField.DnsUpstream],
                onNext = next,
                hint = "One per line, asked directly. Used when its answer is in the local region.",
                singleLine = false,
                keyboardType = KeyboardType.Uri,
            )
            Field(
                label = "Alternative DNS",
                value = dnsAlternative,
                onValueChange = { dnsAlternative = it },
                error = errors[ProfileField.DnsAlternative],
                imeAction = ImeAction.Done,
                onDone = ::save,
                hint = "IPv4 addresses, one per line, asked through the tunnel. Used otherwise.",
                singleLine = false,
                keyboardType = KeyboardType.Decimal,
            )
            saveError?.let { Text("Could not save: $it") }
            Row(
                modifier = Modifier.fillMaxWidth(),
                horizontalArrangement = Arrangement.spacedBy(12.dp, Alignment.End),
                verticalAlignment = Alignment.CenterVertically,
            ) {
                if (saving) CircularProgressIndicator(Modifier.size(24.dp))
                Button(onClick = ::save, enabled = !saving) {
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
    imeAction: ImeAction = ImeAction.Next,
    visualTransformation: VisualTransformation = VisualTransformation.None,
    onNext: () -> Unit = {},
    onDone: () -> Unit = {},
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
        maxLines = if (singleLine) 1 else 4,
        visualTransformation = visualTransformation,
        keyboardOptions = KeyboardOptions(keyboardType = keyboardType, imeAction = imeAction),
        keyboardActions = KeyboardActions(onNext = { onNext() }, onDone = { onDone() }),
    )
}
