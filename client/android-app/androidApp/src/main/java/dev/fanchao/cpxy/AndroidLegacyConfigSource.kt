package dev.fanchao.cpxy

import android.content.Context
import dev.fanchao.cpxy.app.LegacyConfigSource

internal const val LEGACY_PREFERENCES_NAME = "default"
internal const val LEGACY_CONFIG_KEY = "config"

class AndroidLegacyConfigSource(
    context: Context,
) : LegacyConfigSource {
    private val preferences = context.applicationContext.getSharedPreferences(
        LEGACY_PREFERENCES_NAME,
        Context.MODE_PRIVATE,
    )

    override suspend fun read(): String? = preferences.getString(LEGACY_CONFIG_KEY, null)
}
