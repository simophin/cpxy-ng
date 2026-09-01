package dev.fanchao.cpxy.shared

/** Build-independent metadata exposed by the shared application module. */
data class SharedModuleInfo(
    val name: String,
    val supportedPlatforms: Set<String>,
)

object SharedModule {
    val info = SharedModuleInfo(
        name = "Cpxy",
        supportedPlatforms = setOf("Android", "Desktop"),
    )
}
