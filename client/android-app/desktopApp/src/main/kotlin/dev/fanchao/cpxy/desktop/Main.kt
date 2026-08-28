package dev.fanchao.cpxy.desktop

import androidx.compose.material.MaterialTheme
import androidx.compose.material.Surface
import androidx.compose.material.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.window.Window
import androidx.compose.ui.window.application
import dev.zacsweers.metro.createGraphFactory
import java.nio.file.Path

fun main() {
    val home = System.getProperty("user.home")
    val graph = createGraphFactory<DesktopAppGraph.Factory>().create(
        appPaths = AppPaths(Path.of(home, ".cpxy").toString()),
        nativeLibraryPath = NativeLibraryPath("bundled native client"),
    )

    try {
        application {
            Window(
                onCloseRequest = ::exitApplication,
                title = "Cpxy",
            ) {
                App()
            }
        }
    } finally {
        graph.appLifecycle.close()
    }
}

@Composable
private fun App() {
    MaterialTheme {
        Surface {
            androidx.compose.foundation.layout.Box(
                contentAlignment = Alignment.Center,
            ) {
                Text("Cpxy Desktop")
            }
        }
    }
}
