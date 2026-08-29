package dev.fanchao.cpxy.desktop

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Public
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.graphics.vector.rememberVectorPainter
import androidx.compose.ui.window.Notification
import androidx.compose.ui.window.Tray
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import androidx.compose.ui.window.isTraySupported
import androidx.compose.ui.window.rememberTrayState
import dev.fanchao.cpxy.app.ConfigLoadState
import dev.fanchao.cpxy.ui.CpxyApp
import dev.fanchao.cpxy.ui.events.JvmEventTextFormatter
import dev.fanchao.cpxy.ui.theme.CpxyTheme
import dev.zacsweers.metro.createGraphFactory
import kotlinx.coroutines.launch

fun main(args: Array<String>) {
    if (args.contentEquals(arrayOf("--native-probe"))) {
        NativeSmokeProbe.run()
        return
    }
    require(args.isEmpty()) {
        "Unknown Desktop arguments: ${args.joinToString(" ")}"
    }

    launchDesktopUi()
}

private fun launchDesktopUi() {
    val graph = createGraphFactory<DesktopAppGraph.Factory>().create(
        appPaths = AppPaths.forSystem(),
        nativeLibraryPath = NativeLibraryResolver.resolve(),
    )

    try {
        application {
            var isWindowVisible by remember { mutableStateOf(true) }
            var windowActivationRequest by remember { mutableIntStateOf(0) }
            val configLoadState by graph.appController.configRepository.loadState.collectAsState()
            val coroutineScope = rememberCoroutineScope()
            val trayState = rememberTrayState()

            fun showWindow() {
                isWindowVisible = true
                windowActivationRequest++
            }

            fun toggleProfile(profileId: String) {
                val currentProfileId = (configLoadState as? ConfigLoadState.Loaded)
                    ?.config
                    ?.enabledProfileId
                coroutineScope.launch {
                    runCatching {
                        graph.appController.configRepository.setProfileEnabled(
                            toggledProfileId(currentProfileId, profileId),
                        )
                    }.onFailure { error ->
                        trayState.sendNotification(
                            Notification(
                                title = "Cpxy",
                                message = error.message ?: "Unable to switch server",
                                type = Notification.Type.Error,
                            ),
                        )
                    }
                }
            }

            Tray(
                icon = rememberVectorPainter(Icons.Default.Public),
                state = trayState,
                tooltip = trayTooltip(configLoadState),
                onAction = ::showWindow,
            ) {
                Item("Open Cpxy", onClick = ::showWindow)
                Separator()

                when (val current = configLoadState) {
                    ConfigLoadState.Loading -> Item("Loading servers…", enabled = false) {}
                    is ConfigLoadState.Error -> Item("Servers unavailable", enabled = false) {}
                    is ConfigLoadState.Loaded -> {
                        if (current.config.profiles.isEmpty()) {
                            Item("No servers configured", enabled = false) {}
                        } else {
                            current.config.profiles.forEach { profile ->
                                CheckboxItem(
                                    text = profile.name,
                                    checked = current.config.enabledProfileId == profile.id,
                                    onCheckedChange = { toggleProfile(profile.id) },
                                )
                            }
                        }
                    }
                }

                Separator()
                Item("Quit Cpxy", onClick = ::exitApplication)
            }

            Window(
                onCloseRequest = {
                    if (isTraySupported) isWindowVisible = false else exitApplication()
                },
                title = "Cpxy",
                visible = isWindowVisible,
            ) {
                LaunchedEffect(windowActivationRequest) {
                    if (isWindowVisible) {
                        window.toFront()
                        window.requestFocus()
                    }
                }
                val eventTextFormatter = remember { JvmEventTextFormatter() }
                CpxyTheme {
                    CpxyApp(
                        controller = graph.appController,
                        eventTextFormatter = eventTextFormatter,
                    )
                }
            }
        }
    } finally {
        graph.appLifecycle.close()
    }
}

internal fun toggledProfileId(enabledProfileId: String?, selectedProfileId: String): String? =
    selectedProfileId.takeUnless { it == enabledProfileId }

private fun trayTooltip(loadState: ConfigLoadState): String = when (loadState) {
    ConfigLoadState.Loading -> "Cpxy — Loading"
    is ConfigLoadState.Error -> "Cpxy — Configuration unavailable"
    is ConfigLoadState.Loaded -> loadState.config.enabledProfile
        ?.let { "Cpxy — ${it.name}" }
        ?: "Cpxy — Disabled"
}
