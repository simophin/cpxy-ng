package dev.fanchao.cpxy

import com.sun.jna.Library
import com.sun.jna.NativeLong
import com.sun.jna.Pointer


interface Client : Library {
    fun create_client(
        httpProxyPort: Short,
        socks5ProxyPort: Short,
        apiServerPort: Short,
        mainServerUrl: String,
        aiServerUrl: String?,
        tailscaleServerUrl: String?,
        errorMessage: ByteArray,
        errorMessageLen: NativeLong
    ): Pointer?

    fun destroy_client(instance: Pointer)
}

fun Client.create(
    httpProxyPort: UShort,
    socks5ProxyPort: UShort,
    apiServerPort: UShort,
    mainServerUrl: String,
    aiServerUrl: String?,
    tailscaleServerUrl: String?,
): Pointer {
    val errorMessage = ByteArray(512)

    val ptr = create_client(
        httpProxyPort = httpProxyPort.toShort(),
        socks5ProxyPort = socks5ProxyPort.toShort(),
        apiServerPort = apiServerPort.toShort(),
        mainServerUrl = mainServerUrl,
        aiServerUrl = aiServerUrl,
        tailscaleServerUrl = tailscaleServerUrl,
        errorMessage = errorMessage,
        errorMessageLen = NativeLong(errorMessage.size.toLong())
    )

    if (ptr == null || ptr.getInt(0) == 0) {
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

    return ptr
}

fun Client.destroy(instance: Pointer) = destroy_client(instance)