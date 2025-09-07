package dev.fanchao.cpxy.ui

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.ArrowBack
import androidx.compose.material.icons.filled.Done
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.App.Companion.appInstance
import dev.fanchao.cpxy.ClientConfiguration
import dev.fanchao.cpxy.isValidBindAddress
import kotlinx.serialization.Serializable
import java.util.UUID

@Serializable
data class EditConfigRoute(val id: String?)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EditConfigScreen(
    configId: String?,
    onDone: () -> Unit,
) {
    val context = LocalContext.current
    val config = remember {
        context.appInstance.configurationRepository.configurations.value.firstOrNull { it.id == configId }
    }
    val nameState = remember {
        EditingState(config?.name.orEmpty(), label = "Name", validator = nonEmptyValidator("Name") )
    }

    val serverHostState = remember {
        EditingState(config?.serverHost.orEmpty(), label = "Server Host", validator = nonEmptyValidator("Server Host") )
    }

    val serverPortState = remember {
        EditingState(
            initialText = config?.serverPort?.takeIf { it > 0.toUShort() }?.toString().orEmpty(),
            label = "Server Port",
            validator = {
                if ((it.toUShortOrNull()?.toInt() ?: 0) == 0) {
                    "Port must be a number between 1 and 65535"
                } else {
                    null
                }
            }
        )
    }

    val keyState = remember {
        EditingState(config?.key.orEmpty(), label = "Key", validator = nonEmptyValidator("Key") )
    }

    val bindAddressState = remember {
        EditingState(config?.bindAddress.orEmpty(), label = "Bind address", validator = {
            if (isValidBindAddress(it)) {
                null
            } else {
                "Bind address must be in format of host:port, e.g. 127.0.0.1:80"
            }
        })
    }

    val allFieldStates = listOf(
        nameState,
        serverHostState,
        serverPortState,
        keyState,
        bindAddressState,
    )

    val onSave = {
        val isValid = allFieldStates.fold(true) { acc, state ->
            val v = state.validate()
            acc && v
        }

        if (isValid) {
            context.appInstance.configurationRepository
                .save(ClientConfiguration(
                    id = configId ?: UUID.randomUUID().toString(),
                    name = nameState.text.value,
                    serverHost = serverHostState.text.value,
                    serverPort = serverPortState.text.value.toUShort(),
                    key = keyState.text.value,
                    bindAddress = bindAddressState.text.value,
                ))
            onDone()
        }
    }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Edit") },
                navigationIcon = {
                    IconButton(onClick = onDone) {
                        Icon(Icons.AutoMirrored.Default.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    IconButton(onClick = onSave) {
                        Icon(Icons.Default.Done, contentDescription = "Save")
                    }
                }
            )
        }
    ) { paddings ->
        EditConfig(
            modifier = Modifier
                .padding(paddings)
                .padding(16.dp)
                .fillMaxSize(),
            states = allFieldStates
        )
    }
}

fun nonEmptyValidator(label: String): (String) -> String? {
    return { input ->
        if (input.isBlank()) {
            "$label can't be empty"
        } else {
            null
        }
    }
}

class EditingState(
    initialText: String,
    val label: String,
    private val validator: (String) -> String?,
) {
    val text: MutableState<String> = mutableStateOf(initialText)
    val error: MutableState<String?> = mutableStateOf(null)

    fun validate(): Boolean {
        error.value = validator(text.value)
        return error.value == null
    }
}

@Composable
private fun EditConfig(
    modifier: Modifier = Modifier,
    states: List<EditingState>,
) {
    Column(modifier = modifier
        .verticalScroll(rememberScrollState())) {
        for (state in states) {
            OutlinedTextField(
                modifier = Modifier.fillMaxWidth(),
                value = state.text.value,
                singleLine = true,
                onValueChange = {
                    state.text.value = it
                    state.error.value = null
                },
                label = { Text(state.label) },
                isError = state.error.value != null,
                supportingText = {
                    if (state.error.value != null) {
                        Text(state.error.value!!)
                    }
                }
            )
        }
    }
}