package dev.fanchao.cpxy.vpn

import androidx.compose.ui.graphics.ImageBitmap

/** What the settings tab can do on the platform. Null where the platform cannot. */
interface Platform {
    /** Opens the system's VPN settings, where the user turns on always-on VPN. */
    val openAlwaysOnSettings: (() -> Unit)?

    /** Asks the system to add the quick settings tile. */
    val addQuickSettingsTile: (() -> Unit)?

    /** The apps to choose from for the [AppFilter]. */
    val apps: AppCatalog?
}

data class InstalledApp(
    val packageName: String,
    val label: String,
    /** A system component rather than an app the user opens; hidden unless asked for. */
    val isSystem: Boolean,
)

interface AppCatalog {
    /** The apps that can use the network, other than this one. */
    suspend fun apps(): List<InstalledApp>

    suspend fun icon(packageName: String): ImageBitmap?
}
