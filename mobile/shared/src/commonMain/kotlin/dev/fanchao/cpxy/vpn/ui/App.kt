package dev.fanchao.cpxy.vpn.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.foundation.layout.Row
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.ui.NavDisplay
import androidx.savedstate.serialization.SavedStateConfiguration
import dev.fanchao.cpxy.vpn.AppFilter
import dev.fanchao.cpxy.vpn.Platform
import dev.fanchao.cpxy.vpn.ProfileRepository
import dev.fanchao.cpxy.vpn.SettingsRepository
import dev.fanchao.cpxy.vpn.UpdateChecker
import dev.fanchao.cpxy.vpn.VpnController
import kotlinx.serialization.Serializable
import kotlinx.serialization.modules.SerializersModule
import kotlinx.serialization.modules.polymorphic
import kotlinx.serialization.modules.subclass

@Serializable
sealed interface Route : NavKey

@Serializable
data object HomeRoute : Route

@Serializable
data class EditProfileRoute(val profileId: String?, val duplicate: Boolean = false) : Route

@Serializable
data class AppPickerRoute(val filter: AppFilter) : Route

private val SavedStateConfig = SavedStateConfiguration {
    serializersModule = SerializersModule {
        polymorphic(NavKey::class) {
            subclass(HomeRoute::class, HomeRoute.serializer())
            subclass(EditProfileRoute::class, EditProfileRoute.serializer())
            subclass(AppPickerRoute::class, AppPickerRoute.serializer())
        }
    }
}

@Composable
fun CpxyVpnApp(
    repository: ProfileRepository,
    settingsRepository: SettingsRepository,
    controller: VpnController,
    platform: Platform,
) {
    MaterialTheme(colorScheme = if (isSystemInDarkTheme()) darkColorScheme() else lightColorScheme()) {
        val backStack = rememberNavBackStack(SavedStateConfig, HomeRoute)
        val pop = { if (backStack.size > 1) backStack.removeAt(backStack.lastIndex) }

        platform.updates?.let { UpdatePrompt(it) }

        NavDisplay(
            backStack = backStack,
            onBack = { pop() },
            entryProvider = entryProvider {
                entry<HomeRoute> {
                    HomeScreen(
                        repository = repository,
                        settingsRepository = settingsRepository,
                        controller = controller,
                        platform = platform,
                        onEditProfile = { backStack += EditProfileRoute(it) },
                        onDuplicateProfile = { backStack += EditProfileRoute(it, duplicate = true) },
                        onChooseApps = { backStack += AppPickerRoute(it) },
                    )
                }
                entry<EditProfileRoute> { route ->
                    EditProfileScreen(
                        profileId = route.profileId,
                        duplicate = route.duplicate,
                        repository = repository,
                        onDone = { pop() },
                    )
                }
                entry<AppPickerRoute> { route ->
                    platform.apps?.let { catalog ->
                        AppPickerScreen(
                            filter = route.filter,
                            settingsRepository = settingsRepository,
                            catalog = catalog,
                            onDone = { pop() },
                        )
                    }
                }
            },
        )
    }
}

/** Runs the daily update check, and offers the version it finds. */
@Composable
private fun UpdatePrompt(updates: UpdateChecker) {
    LaunchedEffect(updates) { updates.checkIfDue() }
    val version = updates.prompt.collectAsState().value ?: return
    AlertDialog(
        onDismissRequest = updates::dismissPrompt,
        title = { Text("Update available") },
        text = { Text("Version $version is available. The VPN reconnects after the update.") },
        confirmButton = { TextButton(onClick = { updates.install(version) }) { Text("Install") } },
        dismissButton = {
            Row {
                TextButton(onClick = { updates.skip(version) }) { Text("Skip this version") }
                TextButton(onClick = updates::dismissPrompt) { Text("Later") }
            }
        },
    )
}
