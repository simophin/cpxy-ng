package dev.fanchao.cpxy

import kotlinx.serialization.Serializable

@Serializable
data class ClientConfiguration(
    val id: String,
    val name: String,
    val serverHost: String,
    val serverPort: UShort,
    val key: String,
    val bindAddress: String,
    val enabled: Boolean,
) {
    val isValid: Boolean
        get() = name.isNotBlank() &&
                serverHost.isNotBlank() &&
                serverPort.toInt() != 0 &&
                key.isNotBlank() &&
                bindAddress.isNotBlank()
}


fun isValidBindAddress(text: String): Boolean {
    val splits = text.split(':')
    if (splits.size != 2) {
        return false
    }
    val (host, port) = splits
    val portNum = port.toUShortOrNull()?.toUInt() ?: 0.toUInt()
    return host.isNotBlank() && portNum != 0.toUInt()
}