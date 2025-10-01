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
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.ConfigRepository
import kotlinx.serialization.Serializable

@Serializable
data object ProxyConfigRoute

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ProxyConfigScreen(
    repo: ConfigRepository,
    onDone: () -> Unit,
) {
    val context = LocalContext.current

    val httpProxyPort =
        remember { mutableStateOf(repo.clientConfig.value.httpProxyPort.toString()) }
    val socksProxyPort =
        remember { mutableStateOf(repo.clientConfig.value.socks5ProxyPort.toString()) }

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
            onDone()
        } catch (e: Exception) {
            Toast.makeText(context, e.message, Toast.LENGTH_SHORT).show()
        }
    }


    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Settings") },
                navigationIcon = {
                    IconButton(onClick = onDone) {
                        Icon(Icons.AutoMirrored.Default.ArrowBack, contentDescription = "Back")
                    }
                },
                actions = {
                    IconButton(onClick = save) {
                        Icon(Icons.Default.Done, contentDescription = "Save")
                    }
                }
            )
        }
    ) { padding ->
        Column(
            modifier = Modifier
                .padding(padding)
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
        }
    }
}