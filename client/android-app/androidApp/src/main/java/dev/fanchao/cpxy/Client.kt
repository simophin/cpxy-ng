package dev.fanchao.cpxy

import com.sun.jna.Library
import com.sun.jna.Pointer
import java.util.concurrent.atomic.AtomicReference

interface Client : Library {
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

class JnaNativeClientSession internal constructor(
    private val client: Client,
    pointer: Pointer,
) : dev.fanchao.cpxy.app.NativeClientSession {
    private val pointer = AtomicReference(pointer)

    override fun close() {
        pointer.getAndSet(null)?.let(client::cpxy_client_destroy)
    }
}

fun Client.create(
    httpProxyPort: UShort,
    socks5ProxyPort: UShort,
    apiServerPort: UShort,
    dnsServer: String,
    mainServerUrl: String,
    aiServerUrl: String?,
    tailscaleServerUrl: String?,
): JnaNativeClientSession {
    val actualAbiVersion = cpxy_client_abi_version()
    check(actualAbiVersion == CPXY_CLIENT_ABI_VERSION) {
        "Unsupported native client ABI version $actualAbiVersion; expected $CPXY_CLIENT_ABI_VERSION"
    }

    val errorMessage = ByteArray(512)

    val pointer = cpxy_client_create(
        httpProxyPort = httpProxyPort.toShort(),
        socks5ProxyPort = socks5ProxyPort.toShort(),
        apiServerPort = apiServerPort.toShort(),
        mainServerUrl = mainServerUrl,
        aiServerUrl = aiServerUrl,
        tailscaleServerUrl = tailscaleServerUrl,
        errorMessage = errorMessage,
        errorMessageCapacity = errorMessage.size,
        dnsServer = dnsServer,
    )

    if (pointer == null) {
        val realErrorMessageLength = errorMessage.indexOfFirst { it.toInt() == 0 }
            .takeIf { it >= 0 }
            ?: errorMessage.size

        throw RuntimeException(
            String(
                errorMessage,
                0,
                realErrorMessageLength,
                Charsets.UTF_8
            )
        )
    }

    return JnaNativeClientSession(this, pointer)
}

private const val CPXY_CLIENT_ABI_VERSION = 1
