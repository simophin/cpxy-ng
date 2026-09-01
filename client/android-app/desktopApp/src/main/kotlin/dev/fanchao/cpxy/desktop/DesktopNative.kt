package dev.fanchao.cpxy.desktop

import com.sun.jna.Library
import com.sun.jna.Native
import com.sun.jna.Pointer
import dev.fanchao.cpxy.app.AppScope
import dev.fanchao.cpxy.app.NativeClient
import dev.fanchao.cpxy.app.NativeClientConfig
import dev.fanchao.cpxy.app.NativeClientSession
import dev.zacsweers.metro.ContributesBinding
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import java.nio.file.Files
import java.nio.file.Path
import java.util.Locale
import java.util.concurrent.atomic.AtomicReference

internal const val NATIVE_LIBRARY_PATH_PROPERTY = "cpxy.native.library.path"
internal const val COMPOSE_RESOURCES_DIRECTORY_PROPERTY = "compose.application.resources.dir"
internal const val CPXY_CLIENT_ABI_VERSION = 1

data class DesktopPlatform(
    val id: String,
    val libraryName: String,
    val osName: String,
    val architecture: String,
) {
    companion object {
        fun detect(osName: String, architecture: String): DesktopPlatform {
            val os = osName.lowercase(Locale.ROOT)
            val arch = architecture.lowercase(Locale.ROOT)
            val (id, library) = when {
                os.contains("linux") && arch in setOf("amd64", "x86_64") ->
                    "linux-x64" to "libclient.so"
                os.contains("windows") && arch in setOf("amd64", "x86_64") ->
                    "windows-x64" to "client.dll"
                (os.contains("mac") || os.contains("darwin")) && arch in setOf("amd64", "x86_64") ->
                    "macos-x64" to "libclient.dylib"
                (os.contains("mac") || os.contains("darwin")) && arch in setOf("aarch64", "arm64") ->
                    "macos-arm64" to "libclient.dylib"
                else -> throw IllegalStateException(
                    "Unsupported Desktop native platform: OS='$osName', architecture='$architecture'. " +
                        "Supported platforms are Windows x64, Linux x64, macOS x64, and macOS arm64."
                )
            }
            return DesktopPlatform(id, library, osName, architecture)
        }

        fun current(): DesktopPlatform = detect(
            System.getProperty("os.name"),
            System.getProperty("os.arch"),
        )
    }
}

data class NativeLibraryPath(val value: Path, val platform: DesktopPlatform)

object NativeLibraryResolver {
    fun resolve(
        platform: DesktopPlatform = DesktopPlatform.current(),
        properties: Map<String, String> = systemProperties(),
        isRegularFile: (Path) -> Boolean = Files::isRegularFile,
    ): NativeLibraryPath {
        val explicitPath = properties[NATIVE_LIBRARY_PATH_PROPERTY]?.takeIf(String::isNotBlank)
        val packagedDirectory = properties[COMPOSE_RESOURCES_DIRECTORY_PROPERTY]
            ?.takeIf(String::isNotBlank)
        val candidate = when {
            explicitPath != null -> Path.of(explicitPath)
            packagedDirectory != null -> Path.of(packagedDirectory).resolve(platform.libraryName)
            else -> throw IllegalStateException(
                "Desktop native library location is not configured for OS='${platform.osName}', " +
                    "architecture='${platform.architecture}'. Expected an absolute path in " +
                    "'$NATIVE_LIBRARY_PATH_PROPERTY' or a packaged resources directory in " +
                    "'$COMPOSE_RESOURCES_DIRECTORY_PROPERTY' containing '${platform.libraryName}'."
            )
        }.toAbsolutePath().normalize()

        if (!isRegularFile(candidate)) {
            throw IllegalStateException(
                "Desktop native library is missing. Expected path='$candidate', " +
                    "OS='${platform.osName}', architecture='${platform.architecture}', " +
                    "library='${platform.libraryName}'. Run the app with the Gradle " +
                    "desktopApp:run task so the Rust library is generated."
            )
        }
        return NativeLibraryPath(candidate, platform)
    }

    private fun systemProperties(): Map<String, String> = buildMap {
        System.getProperty(NATIVE_LIBRARY_PATH_PROPERTY)?.let {
            put(NATIVE_LIBRARY_PATH_PROPERTY, it)
        }
        System.getProperty(COMPOSE_RESOURCES_DIRECTORY_PROPERTY)?.let {
            put(COMPOSE_RESOURCES_DIRECTORY_PROPERTY, it)
        }
    }
}

internal interface DesktopClientLibrary : Library {
    fun cpxy_client_abi_version(): Int

    fun cpxy_client_create(
        httpProxyPort: Short,
        socks5ProxyPort: Short,
        apiServerPort: Short,
        dnsServer: String,
        mainServerUrl: String,
        aiServerUrl: String?,
        tailscaleServerUrl: String?,
        errorMessage: ByteArray,
        errorMessageCapacity: Int,
    ): Pointer?

    fun cpxy_client_destroy(instance: Pointer?)
}

internal object JnaDesktopClientLibraryLoader {
    fun load(path: NativeLibraryPath): DesktopClientLibrary = try {
        Native.load(path.value.toString(), DesktopClientLibrary::class.java)
    } catch (error: Throwable) {
        throw IllegalStateException(
            "Failed to load Desktop native library. Expected path='${path.value}', " +
                "OS='${path.platform.osName}', architecture='${path.platform.architecture}'. " +
                "Native loader error: ${error.message ?: error::class.java.name}",
            error,
        )
    }
}

@ContributesBinding(AppScope::class)
@SingleIn(AppScope::class)
class DesktopNativeClient internal constructor(
    private val library: DesktopClientLibrary,
    private val libraryDescription: String,
) : NativeClient {
    @Inject
    constructor(nativeLibraryPath: NativeLibraryPath) : this(
        JnaDesktopClientLibraryLoader.load(nativeLibraryPath),
        "path='${nativeLibraryPath.value}', OS='${nativeLibraryPath.platform.osName}', " +
            "architecture='${nativeLibraryPath.platform.architecture}'",
    )

    init {
        val actualVersion = try {
            library.cpxy_client_abi_version()
        } catch (error: Throwable) {
            throw IllegalStateException(
                "Could not read Desktop native ABI version from $libraryDescription: " +
                    "${error.message ?: error::class.java.name}",
                error,
            )
        }
        check(actualVersion == CPXY_CLIENT_ABI_VERSION) {
            "Unsupported Desktop native ABI version $actualVersion; expected " +
                "$CPXY_CLIENT_ABI_VERSION ($libraryDescription)."
        }
    }

    override fun start(config: NativeClientConfig): NativeClientSession {
        val errorMessage = ByteArray(512)
        val pointer = library.cpxy_client_create(
            httpProxyPort = config.httpProxyPort.toShort(),
            socks5ProxyPort = config.socks5ProxyPort.toShort(),
            apiServerPort = config.apiServerPort.toShort(),
            dnsServer = config.dnsServer,
            mainServerUrl = config.mainServerUrl,
            aiServerUrl = config.aiServerUrl,
            tailscaleServerUrl = config.tailscaleServerUrl,
            errorMessage = errorMessage,
            errorMessageCapacity = errorMessage.size,
        ) ?: throw IllegalStateException(decodeError(errorMessage))
        return DesktopNativeClientSession(library, pointer)
    }

    private fun decodeError(bytes: ByteArray): String {
        val length = bytes.indexOfFirst { it == 0.toByte() }.takeIf { it >= 0 } ?: bytes.size
        return String(bytes, 0, length, Charsets.UTF_8).ifBlank {
            "Native client creation failed without an error message ($libraryDescription)."
        }
    }
}

internal class DesktopNativeClientSession(
    private val library: DesktopClientLibrary,
    pointer: Pointer,
) : NativeClientSession {
    private val pointer = AtomicReference(pointer)

    override fun close() {
        pointer.getAndSet(null)?.let(library::cpxy_client_destroy)
    }
}

object NativeSmokeProbe {
    @JvmStatic
    fun main(args: Array<String>) {
        require(args.isEmpty()) { "Native probe does not accept arguments" }
        run()
    }

    fun run() {
        val path = NativeLibraryResolver.resolve()
        val client = DesktopNativeClient(path)
        val failure = runCatching {
            client.start(
                NativeClientConfig(
                    httpProxyPort = 0u,
                    socks5ProxyPort = 0u,
                    apiServerPort = 0u,
                    dnsServer = "not-an-ip-address",
                    mainServerUrl = "invalid",
                    aiServerUrl = null,
                    tailscaleServerUrl = null,
                )
            )
        }.exceptionOrNull()
        check(failure != null) { "Native error-path probe unexpectedly created a session" }
        println("Desktop native ABI $CPXY_CLIENT_ABI_VERSION loaded from ${path.value}; error path passed")
    }
}
