package dev.fanchao.cpxy

data class ClientConfiguration(
    val serverHost: String,
    val serverPort: Short,
    val useWebsocket: Boolean,
    val key: String,
    val bindAddress: String,
)
