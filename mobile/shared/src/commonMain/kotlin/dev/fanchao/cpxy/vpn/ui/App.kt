package dev.fanchao.cpxy.vpn.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.ui.NavDisplay
import androidx.savedstate.serialization.SavedStateConfiguration
import dev.fanchao.cpxy.vpn.AppFilter
import dev.fanchao.cpxy.vpn.Platform
import dev.fanchao.cpxy.vpn.ProfileRepository
import dev.fanchao.cpxy.vpn.SettingsRepository
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
data class EditProfileRoute(val profileId: String?) : Route

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
                        onChooseApps = { backStack += AppPickerRoute(it) },
                    )
                }
                entry<EditProfileRoute> { route ->
                    EditProfileScreen(
                        profileId = route.profileId,
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
