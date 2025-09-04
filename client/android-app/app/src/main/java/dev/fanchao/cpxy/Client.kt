package dev.fanchao.cpxy

import com.sun.jna.Library
import com.sun.jna.NativeLong
import com.sun.jna.Pointer

interface Client : Library {
    fun client_create(
        serverHost: String,
        serverPort: Short,
        key: String,
        bindAddress: String,
        useWebSocket: Boolean,
        errorMessage: ByteArray,
        errorMessageLen: NativeLong
    ): Pointer?

    fun client_destroy(instance: Pointer)
}

fun Client.create(serverHost: String,
                  serverPort: Short,
                  key: String,
                  bindAddress: String,
                  useWebSocket: Boolean): Pointer {
    val errorMessage = ByteArray(512)

    val ptr = client_create(
        serverHost = serverHost,
        serverPort = serverPort,
        key = key,
        bindAddress = bindAddress,
        useWebSocket = useWebSocket,
        errorMessage = errorMessage,
        errorMessageLen = NativeLong(errorMessage.size.toLong())
    )

    if (ptr == null || ptr.getInt(0) == 0) {
        val realErrorMessageLength = errorMessage.indexOfFirst { it.toInt() == 0 }
            .takeIf { it >= 0 }
            ?: errorMessage.size

        throw RuntimeException(String(
            errorMessage,
            0,
            realErrorMessageLength,
            Charsets.UTF_8
        ))
    }

    return ptr
}

fun Client.destroy(instance: Pointer) = client_destroy(instance)