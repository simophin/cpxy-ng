package dev.fanchao.cpxy.desktop

import androidx.compose.runtime.remember
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import dev.fanchao.cpxy.ui.CpxyApp
import dev.fanchao.cpxy.ui.events.JvmEventTextFormatter
import dev.fanchao.cpxy.ui.theme.CpxyTheme
import dev.zacsweers.metro.createGraphFactory

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
