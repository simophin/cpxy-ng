package dev.fanchao.cpxy.rtp

import androidx.compose.ui.graphics.ImageBitmap

/** What the UI needs from the platform. */
interface Platform {
    /** The apps to choose from for the [AppFilter]; null where the platform has no per-app VPN. */
    val apps: AppCatalog?

    val keys: KeyVault

    /** The app's version name, such as `1.2.0` or `dev`. */
    val versionName: String
}

/** Keeps each profile's tunnel key, derived from its password, in the platform's secure storage. */
interface KeyVault {
    /** Derives the key from [password] and stores it for the profile. Slow: Argon2id. */
    suspend fun store(profileId: String, password: String)

    suspend fun contains(profileId: String): Boolean

    suspend fun delete(profileId: String)
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
