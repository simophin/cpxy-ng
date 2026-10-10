package dev.fanchao.cpxy.vpn

import android.Manifest
import android.app.Activity
import android.app.StatusBarManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.graphics.drawable.Icon
import android.os.Build
import android.provider.Settings
import android.util.LruCache
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.core.graphics.drawable.toBitmap
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/** The settings tab's system actions, run from [activity]. */
class AndroidPlatform(private val activity: Activity) : Platform {
    override val openAlwaysOnSettings: () -> Unit = {
        activity.startActivity(Intent(Settings.ACTION_VPN_SETTINGS))
    }

    override val addQuickSettingsTile: (() -> Unit)? =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            {
                activity.getSystemService(StatusBarManager::class.java).requestAddTileService(
                    ComponentName(activity, CpxyTileService::class.java),
                    activity.getString(R.string.app_name),
                    Icon.createWithResource(activity, R.drawable.ic_vpn_key),
                    activity.mainExecutor,
                ) {}
            }
        } else {
            // Users add it by editing the quick settings panel.
            null
        }

    override val apps: AppCatalog = AndroidAppCatalog(activity.applicationContext)
}

private class AndroidAppCatalog(private val context: Context) : AppCatalog {
    private val packageManager = context.packageManager
    private val icons = LruCache<String, ImageBitmap>(200)

    override suspend fun apps(): List<InstalledApp> = withContext(Dispatchers.IO) {
        installedPackages()
            .filter { info ->
                info.packageName != context.packageName &&
                    info.requestedPermissions?.contains(Manifest.permission.INTERNET) == true
            }
            .mapNotNull { info ->
                val application = info.applicationInfo ?: return@mapNotNull null
                InstalledApp(
                    packageName = info.packageName,
                    label = application.loadLabel(packageManager).toString(),
                    // Preinstalled apps the user opens, like the browser, are not hidden.
                    isSystem = application.flags and ApplicationInfo.FLAG_SYSTEM != 0 &&
                        packageManager.getLaunchIntentForPackage(info.packageName) == null,
                )
            }
    }

    private fun installedPackages(): List<PackageInfo> =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            packageManager.getInstalledPackages(
                PackageManager.PackageInfoFlags.of(PackageManager.GET_PERMISSIONS.toLong()),
            )
        } else {
            @Suppress("DEPRECATION")
            packageManager.getInstalledPackages(PackageManager.GET_PERMISSIONS)
        }

    override suspend fun icon(packageName: String): ImageBitmap? {
        icons.get(packageName)?.let { return it }
        return withContext(Dispatchers.IO) {
            try {
                val size = (40 * context.resources.displayMetrics.density).toInt()
                packageManager.getApplicationIcon(packageName).toBitmap(size, size).asImageBitmap()
                    .also { icons.put(packageName, it) }
            } catch (_: PackageManager.NameNotFoundException) {
                null
            }
        }
    }
}
