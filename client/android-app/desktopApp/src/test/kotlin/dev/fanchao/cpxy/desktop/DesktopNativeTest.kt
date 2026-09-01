package dev.fanchao.cpxy.desktop

import com.sun.jna.Pointer
import dev.fanchao.cpxy.app.NativeClientConfig
import java.nio.file.Path
import kotlin.test.Test
import kotlin.test.assertContains
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class DesktopNativeTest {
    private val linux = DesktopPlatform.detect("Linux", "amd64")

    @Test
    fun detectsSupportedPlatforms() {
        assertEquals("linux-x64", linux.id)
        assertEquals("windows-x64", DesktopPlatform.detect("Windows 11", "x86_64").id)
        assertEquals("macos-x64", DesktopPlatform.detect("Mac OS X", "amd64").id)
        assertEquals("macos-arm64", DesktopPlatform.detect("Mac OS X", "aarch64").id)
    }

    @Test
    fun unsupportedPlatformHasActionableDiagnostic() {
        val error = assertFailsWith<IllegalStateException> {
            DesktopPlatform.detect("Plan 9", "mips")
        }
        assertContains(error.message.orEmpty(), "Plan 9")
        assertContains(error.message.orEmpty(), "mips")
    }

    @Test
    fun resolvesExplicitDevelopmentPath() {
        val expected = Path.of("build/dev/libclient.so").toAbsolutePath().normalize()
        val result = NativeLibraryResolver.resolve(
            platform = linux,
            properties = mapOf(NATIVE_LIBRARY_PATH_PROPERTY to expected.toString()),
            isRegularFile = { it == expected },
        )
        assertEquals(expected, result.value)
    }

    @Test
    fun explicitPathTakesPrecedenceOverPackagedResources() {
        val expected = Path.of("build/dev/libclient.so").toAbsolutePath().normalize()
        val result = NativeLibraryResolver.resolve(
            platform = linux,
            properties = mapOf(
                NATIVE_LIBRARY_PATH_PROPERTY to expected.toString(),
                COMPOSE_RESOURCES_DIRECTORY_PROPERTY to "/packaged/resources",
            ),
            isRegularFile = { it == expected },
        )
        assertEquals(expected, result.value)
    }

    @Test
    fun resolvesPackagedResourcePath() {
        val directory = Path.of("build/package-resources").toAbsolutePath().normalize()
        val expected = directory.resolve("libclient.so")
        val result = NativeLibraryResolver.resolve(
            platform = linux,
            properties = mapOf(COMPOSE_RESOURCES_DIRECTORY_PROPERTY to directory.toString()),
            isRegularFile = { it == expected },
        )
        assertEquals(expected, result.value)
    }

    @Test
    fun missingLibraryReportsPathAndPlatform() {
        val expected = Path.of("build/missing/libclient.so").toAbsolutePath().normalize()
        val error = assertFailsWith<IllegalStateException> {
            NativeLibraryResolver.resolve(
                platform = linux,
                properties = mapOf(NATIVE_LIBRARY_PATH_PROPERTY to expected.toString()),
                isRegularFile = { false },
            )
        }
        assertContains(error.message.orEmpty(), expected.toString())
        assertContains(error.message.orEmpty(), "Linux")
        assertContains(error.message.orEmpty(), "amd64")
        assertContains(error.message.orEmpty(), "desktopApp:run")
    }

    @Test
    fun abiMismatchIsRejectedBeforeCreate() {
        val library = FakeLibrary(abiVersion = CPXY_CLIENT_ABI_VERSION + 1)
        val error = assertFailsWith<IllegalStateException> {
            DesktopNativeClient(library, "test library")
        }
        assertContains(error.message.orEmpty(), "expected $CPXY_CLIENT_ABI_VERSION")
        assertEquals(0, library.createCalls)
    }

    @Test
    fun sessionCloseIsIdempotent() {
        val library = FakeLibrary()
        val session = DesktopNativeClient(library, "test library").start(testConfig())
        session.close()
        session.close()
        assertEquals(1, library.destroyCalls)
    }

    @Test
    fun nativeCreateErrorIsDecoded() {
        val library = FakeLibrary(createPointer = null, createError = "bad native configuration")
        val error = assertFailsWith<IllegalStateException> {
            DesktopNativeClient(library, "test library").start(testConfig())
        }
        assertEquals("bad native configuration", error.message)
    }

    private fun testConfig() = NativeClientConfig(
        httpProxyPort = 8080u,
        socks5ProxyPort = 1080u,
        apiServerPort = 3000u,
        dnsServer = "1.1.1.1",
        mainServerUrl = "https://example.com",
        aiServerUrl = null,
        tailscaleServerUrl = null,
    )

    private class FakeLibrary(
        private val abiVersion: Int = CPXY_CLIENT_ABI_VERSION,
        private val createPointer: Pointer? = Pointer(1),
        private val createError: String = "",
    ) : DesktopClientLibrary {
        var createCalls = 0
        var destroyCalls = 0

        override fun cpxy_client_abi_version() = abiVersion

        override fun cpxy_client_create(
            httpProxyPort: Short,
            socks5ProxyPort: Short,
            apiServerPort: Short,
            dnsServer: String,
            mainServerUrl: String,
            aiServerUrl: String?,
            tailscaleServerUrl: String?,
            errorMessage: ByteArray,
            errorMessageCapacity: Int,
        ): Pointer? {
            createCalls++
            if (createPointer == null) {
                createError.encodeToByteArray().copyInto(errorMessage)
            }
            return createPointer
        }

        override fun cpxy_client_destroy(instance: Pointer?) {
            destroyCalls++
        }
    }
}
