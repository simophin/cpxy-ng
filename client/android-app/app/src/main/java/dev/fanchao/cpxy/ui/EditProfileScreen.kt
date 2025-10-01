package dev.fanchao.cpxy.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
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
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.ConfigRepository
import dev.fanchao.cpxy.Profile
import kotlinx.serialization.Serializable
import java.util.UUID

@Serializable
data class EditProfileRoute(val id: String?)

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EditProfileScreen(
    profileId: String?,
    configurationRepository: ConfigRepository,
    onDone: () -> Unit,
) {
    val profile = remember {
        configurationRepository.clientConfig.value.profiles.firstOrNull { it.id == profileId }
    }

    val nameState = remember {
        EditingState(profile?.name.orEmpty(), label = "Name", validator = nonEmptyValidator("Name"))
    }

    val mainServerState = remember {
        EditingState(
            profile?.mainServerUrl.orEmpty(),
            label = "Main server URL",
            validator = nonEmptyValidator("Main server URL")
        )
    }

    val aiServerState = remember {
        EditingState(
            profile?.aiServerUrl.orEmpty(),
            label = "AI server URL",
            validator = { null }
        )
    }

    val tailscaleServerState = remember {
        EditingState(
            profile?.tailscaleServerUrl.orEmpty(),
            label = "Tailscale server URL",
            validator = { null }
        )
    }

    val allFieldStates = listOf(
        nameState,
        mainServerState,
        aiServerState,
        tailscaleServerState,
    )


    val onSave = {
        val isValid = allFieldStates.fold(true) { acc, state ->
            val v = state.validate()
            acc && v
        }

        if (isValid) {
            configurationRepository
                .saveProfile(
                    Profile(
                        id = profileId ?: UUID.randomUUID().toString(),
                        name = nameState.text.value,
                        mainServerUrl = mainServerState.text.value,
                        aiServerUrl = aiServerState.text.value.takeIf { it.isNotBlank() },
                        tailscaleServerUrl = tailscaleServerState.text.value.takeIf { it.isNotBlank() }
                    )
                )
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
            states = allFieldStates,
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
    Column(
        modifier = modifier
            .verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(8.dp)
    ) {
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