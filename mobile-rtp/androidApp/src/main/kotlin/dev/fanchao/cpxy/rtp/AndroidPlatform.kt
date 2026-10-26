package dev.fanchao.cpxy.rtp

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.pm.ApplicationInfo
import android.content.pm.PackageInfo
import android.content.pm.PackageManager
import android.os.Build
import android.util.LruCache
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.core.graphics.drawable.toBitmap
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

class AndroidPlatform(activity: Activity) : Platform {
    override val apps: AppCatalog = AndroidAppCatalog(activity.applicationContext)

    override val keys: KeyVault = activity.app.keys

    override val versionName: String =
        activity.packageManager.getPackageInfo(activity.packageName, 0).versionName ?: "dev"
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
