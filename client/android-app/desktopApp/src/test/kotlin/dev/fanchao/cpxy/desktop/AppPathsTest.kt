package dev.fanchao.cpxy.desktop

import java.nio.file.Path
import kotlin.test.Test
import kotlin.test.assertEquals

class AppPathsTest {
    @Test
    fun usesWindowsAppData() {
        assertEquals(
            Path.of("C:/Users/test/AppData/Roaming", "Cpxy"),
            AppPaths.forSystem("Windows 11", mapOf("APPDATA" to "C:/Users/test/AppData/Roaming"), "ignored").configDirectory,
        )
    }

    @Test
    fun usesMacApplicationSupport() {
        assertEquals(
            Path.of("/Users/test", "Library", "Application Support", "Cpxy"),
            AppPaths.forSystem("Mac OS X", emptyMap(), "/Users/test").configDirectory,
        )
    }

    @Test
    fun usesLinuxXdgOrHomeFallback() {
        assertEquals(
            Path.of("/tmp/config", "cpxy"),
            AppPaths.forSystem("Linux", mapOf("XDG_CONFIG_HOME" to "/tmp/config"), "/home/test").configDirectory,
        )
        assertEquals(
            Path.of("/home/test", ".config", "cpxy"),
            AppPaths.forSystem("Linux", emptyMap(), "/home/test").configDirectory,
        )
    }
}
