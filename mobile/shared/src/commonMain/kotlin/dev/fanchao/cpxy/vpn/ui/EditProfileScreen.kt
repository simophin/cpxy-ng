package dev.fanchao.cpxy.vpn.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.consumeWindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.selection.selectable
import androidx.compose.foundation.selection.selectableGroup
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Delete
import androidx.compose.material3.Button
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.RadioButton
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
import androidx.compose.ui.focus.FocusDirection
import androidx.compose.ui.platform.LocalFocusManager
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.vpn.Ipv6Mode
import dev.fanchao.cpxy.vpn.Profile
import dev.fanchao.cpxy.vpn.ProfileField
import dev.fanchao.cpxy.vpn.ProfileRepository
import dev.fanchao.cpxy.vpn.parseServerList
import dev.fanchao.cpxy.vpn.validationErrors
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch
import kotlin.uuid.ExperimentalUuidApi
import kotlin.uuid.Uuid

/** A new profile starts with AliDNS as its upstream DNS. Every query goes to every server, so one each. */
private val DefaultDnsUpstream = listOf("223.5.5.5")

/** And with an overseas resolver asked through the cpxy server as its alternative DNS. */
private val DefaultDnsAlternative = listOf("tcp://1.1.1.1?via=cpxy")

@OptIn(ExperimentalUuidApi::class)
@Composable
fun EditProfileScreen(
    profileId: String?,
    repository: ProfileRepository,
    onDone: () -> Unit,
    duplicate: Boolean = false,
) {
    // Null while loading.
    val initial by produceState<Profile?>(null, profileId, duplicate) {
        val source = profileId?.let { id -> repository.profiles.first().profiles.firstOrNull { it.id == id } }
            ?: Profile(
                id = Uuid.random().toString(),
                name = "",
                server = "",
                dnsUpstream = DefaultDnsUpstream,
                dnsAlternative = DefaultDnsAlternative,
            )
        value = if (duplicate) source.copy(id = Uuid.random().toString(), name = "${source.name} (copy)") else source
    }
    initial?.let { profile ->
        EditProfileForm(
            initial = profile,
            isNew = profileId == null || duplicate,
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
    var name by remember(initial.id) { mutableStateOf(initial.name) }
    var server by remember(initial.id) { mutableStateOf(initial.server) }
    var dnsUpstream by remember(initial.id) { mutableStateOf(initial.dnsUpstream.joinToString("\n")) }
    var dnsAlternative by remember(initial.id) { mutableStateOf(initial.dnsAlternative.joinToString("\n")) }
    var ipv6 by remember(initial.id) { mutableStateOf(initial.ipv6) }
    var showErrors by remember(initial.id) { mutableStateOf(false) }
    val scope = rememberCoroutineScope()
    val focusManager = LocalFocusManager.current

    val profile = initial.copy(
        name = name.trim(),
        server = server.trim(),
        dnsUpstream = parseServerList(dnsUpstream),
        dnsAlternative = parseServerList(dnsAlternative),
        ipv6 = ipv6,
    )
    val errors = if (showErrors) profile.validationErrors() else emptyMap()

    fun save() {
        showErrors = true
        if (profile.validationErrors().isEmpty()) {
            scope.launch {
                repository.save(profile)
                onDone()
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
                label = "Server",
                value = server,
                onValueChange = { server = it },
                error = errors[ProfileField.Server],
                onNext = next,
                hint = "https://:key@example.com",
                keyboardType = KeyboardType.Uri,
            )
            Field(
                label = "Upstream DNS",
                value = dnsUpstream,
                onValueChange = { dnsUpstream = it },
                error = errors[ProfileField.DnsUpstream],
                onNext = next,
                hint = "One per line. Used when all its answers are in CN.",
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
                hint = "One per line. Used otherwise. ?via=cpxy asks through the server (TCP, TLS or HTTPS).",
                singleLine = false,
                keyboardType = KeyboardType.Uri,
            )
            Text("IPv6", style = MaterialTheme.typography.titleSmall)
            Column(Modifier.selectableGroup()) {
                for (mode in Ipv6Mode.entries) {
                    ListItem(
                        modifier = Modifier.fillMaxWidth().selectable(
                            selected = ipv6 == mode,
                            role = Role.RadioButton,
                            onClick = { ipv6 = mode },
                        ),
                        leadingContent = { RadioButton(selected = ipv6 == mode, onClick = null) },
                        headlineContent = { Text(mode.title) },
                        supportingContent = { Text(mode.description) },
                    )
                }
            }
            Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
                Button(onClick = ::save) {
                    Text("Save")
                }
            }
        }
    }
}

private val Ipv6Mode.title: String
    get() = when (this) {
        Ipv6Mode.Full -> "Full"
        Ipv6Mode.Direct -> "Direct only"
        Ipv6Mode.Off -> "Off"
    }

private val Ipv6Mode.description: String
    get() = when (this) {
        Ipv6Mode.Full -> "IPv6 goes direct or through the server, like IPv4. The server must reach IPv6."
        Ipv6Mode.Direct -> "Only IPv6 that goes direct, such as to CN sites. For a server without IPv6."
        Ipv6Mode.Off -> "Apps use IPv4 only."
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
        keyboardOptions = KeyboardOptions(keyboardType = keyboardType, imeAction = imeAction),
        keyboardActions = KeyboardActions(onNext = { onNext() }, onDone = { onDone() }),
    )
}
