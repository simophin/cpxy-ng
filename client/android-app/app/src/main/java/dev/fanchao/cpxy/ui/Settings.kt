package dev.fanchao.cpxy.ui

import android.widget.Toast
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.App.Companion.appInstance
import kotlinx.coroutines.launch


@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun Settings(
    modifier: Modifier = Modifier,
    snackbarHostState: SnackbarHostState
) {
    val context = LocalContext.current
    val repo = context.appInstance.configurationRepository

    val httpProxyPort =
        remember { mutableStateOf(repo.clientConfig.value.httpProxyPort.toString()) }
    val socksProxyPort =
        remember { mutableStateOf(repo.clientConfig.value.socks5ProxyPort.toString()) }

    val scope = rememberCoroutineScope()

    val save = {
        try {
            val httpProxyPort =
                requireNotNull(httpProxyPort.value.toUShortOrNull()?.takeIf { it > 0u }) {
                    "Invalid HTTP Proxy Port"
                }

            val socksProxyPort =
                requireNotNull(socksProxyPort.value.toUShortOrNull()?.takeIf { it > 0u }) {
                    "Invalid SOCKS5 Proxy Port"
                }

            repo.saveProxySettings(httpProxyPort, socksProxyPort)
            scope.launch {
                snackbarHostState.showSnackbar("Settings saved")
            }
        } catch (e: Exception) {
            Toast.makeText(context, e.message, Toast.LENGTH_SHORT).show()
        }
    }

    Column(
        modifier = modifier
            .padding(16.dp)
            .fillMaxSize()
            .verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(16.dp)
    ) {

        OutlinedTextField(
            value = httpProxyPort.value,
            onValueChange = { httpProxyPort.value = it },
            label = { Text("HTTP Proxy Port") },
            modifier = Modifier.fillMaxWidth(),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
        )

        OutlinedTextField(
            modifier = Modifier.fillMaxWidth(),
            value = socksProxyPort.value,
            onValueChange = { socksProxyPort.value = it },
            label = { Text("SOCKS5 Proxy Port") },
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal),
        )

        FilledTonalButton(
            onClick = { save() },
            modifier = Modifier.fillMaxWidth()
        ) {
            Text("Save")
        }
    }

}