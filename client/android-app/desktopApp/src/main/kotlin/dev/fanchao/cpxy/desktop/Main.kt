package dev.fanchao.cpxy.desktop

import androidx.compose.runtime.remember
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import dev.fanchao.cpxy.ui.CpxyApp
import dev.fanchao.cpxy.ui.events.JvmEventTextFormatter
import dev.fanchao.cpxy.ui.theme.CpxyTheme
import dev.zacsweers.metro.createGraphFactory

fun main() {
    val graph = createGraphFactory<DesktopAppGraph.Factory>().create(
        appPaths = AppPaths.forSystem(),
        nativeLibraryPath = NativeLibraryPath("bundled native client"),
    )

    try {
        application {
            Window(
                onCloseRequest = ::exitApplication,
                title = "Cpxy",
            ) {
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
