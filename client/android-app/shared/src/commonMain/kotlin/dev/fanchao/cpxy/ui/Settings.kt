package dev.fanchao.cpxy.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilledTonalButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import dev.fanchao.cpxy.app.ClientConfig
import dev.fanchao.cpxy.app.ConfigLoadState
import dev.fanchao.cpxy.app.ConfigRepository
import kotlinx.coroutines.launch

@Composable
fun Settings(
    modifier: Modifier = Modifier,
    snackbarHostState: SnackbarHostState,
    repository: ConfigRepository,
) {
    val loadState by repository.loadState.collectAsState()
    when (val current = loadState) {
        ConfigLoadState.Loading -> Box(modifier.fillMaxSize(), contentAlignment = Alignment.Center) { CircularProgressIndicator() }
        is ConfigLoadState.Error -> Box(modifier.fillMaxSize(), contentAlignment = Alignment.Center) {
            Text("Unable to load configuration: ${current.cause.message.orEmpty()}")
        }
        is ConfigLoadState.Loaded -> LoadedSettings(modifier, snackbarHostState, repository, current.config)
    }
}

@Composable
private fun LoadedSettings(
    modifier: Modifier,
    snackbarHostState: SnackbarHostState,
    repository: ConfigRepository,
    config: ClientConfig,
) {
    val httpProxyPort = remember(config) { mutableStateOf(config.httpProxyPort.toString()) }
    val socksProxyPort = remember(config) { mutableStateOf(config.socks5ProxyPort.toString()) }
    val dnsServer = remember(config) { mutableStateOf(config.dnsServer) }
    val scope = rememberCoroutineScope()

    fun save() {
        scope.launch {
            runCatching {
                val http = requireNotNull(httpProxyPort.value.toUShortOrNull()?.takeIf { it > 0u }) { "Invalid HTTP Proxy Port" }
                val socks = requireNotNull(socksProxyPort.value.toUShortOrNull()?.takeIf { it > 0u }) { "Invalid SOCKS5 Proxy Port" }
                require(dnsServer.value.isNotBlank()) { "DNS server cannot be empty" }
                repository.saveProxySettings(http, socks, dnsServer.value)
            }.fold(
                onSuccess = { snackbarHostState.showSnackbar("Settings saved") },
                onFailure = { snackbarHostState.showSnackbar(it.message ?: "Unable to save settings") },
            )
        }
    }

    Column(
        modifier = modifier.padding(16.dp).fillMaxSize().verticalScroll(rememberScrollState()),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        OutlinedTextField(httpProxyPort.value, { httpProxyPort.value = it }, label = { Text("HTTP Proxy Port") }, modifier = Modifier.fillMaxWidth(), keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal))
        OutlinedTextField(socksProxyPort.value, { socksProxyPort.value = it }, label = { Text("SOCKS5 Proxy Port") }, modifier = Modifier.fillMaxWidth(), keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Decimal))
        OutlinedTextField(dnsServer.value, { dnsServer.value = it }, label = { Text("DNS server") }, modifier = Modifier.fillMaxWidth())
        FilledTonalButton(onClick = ::save, modifier = Modifier.fillMaxWidth()) { Text("Save") }
    }
}
