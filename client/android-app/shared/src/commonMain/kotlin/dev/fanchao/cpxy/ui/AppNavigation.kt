package dev.fanchao.cpxy.ui

import androidx.compose.runtime.Composable
import androidx.navigation3.runtime.NavKey
import androidx.navigation3.runtime.entryProvider
import androidx.navigation3.runtime.rememberNavBackStack
import androidx.navigation3.ui.NavDisplay
import androidx.savedstate.serialization.SavedStateConfiguration
import dev.fanchao.cpxy.app.AppController
import dev.fanchao.cpxy.ui.events.EventTextFormatter
import kotlinx.serialization.Serializable
import kotlinx.serialization.modules.SerializersModule
import kotlinx.serialization.modules.polymorphic
import kotlinx.serialization.modules.subclass

@Serializable
sealed interface AppRoute : NavKey

@Serializable
data object HomeRoute : AppRoute

@Serializable
data class EditProfileRoute(
    val profileId: String?,
) : AppRoute

internal val AppNavigationSavedStateConfiguration = SavedStateConfiguration {
    serializersModule = SerializersModule {
        polymorphic(NavKey::class) {
            subclass(HomeRoute::class, HomeRoute.serializer())
            subclass(EditProfileRoute::class, EditProfileRoute.serializer())
        }
    }
}

internal fun <T> MutableList<T>.popUnlessRoot(): Boolean {
    if (size <= 1) return false
    removeAt(lastIndex)
    return true
}

@Composable
fun CpxyApp(
    controller: AppController,
    eventTextFormatter: EventTextFormatter,
) {
    val backStack = rememberNavBackStack(
        configuration = AppNavigationSavedStateConfiguration,
        HomeRoute,
    )

    NavDisplay(
        backStack = backStack,
        onBack = { backStack.popUnlessRoot() },
        entryProvider = entryProvider {
            entry<HomeRoute> {
                HomeScreen(
                    controller = controller,
                    eventTextFormatter = eventTextFormatter,
                    navigateToEditScreen = { backStack += EditProfileRoute(it.id) },
                    navigateToNewConfigScreen = { backStack += EditProfileRoute(null) },
                )
            }
            entry<EditProfileRoute> { route ->
                EditProfileScreen(
                    profileId = route.profileId,
                    configurationRepository = controller.configRepository,
                    onDone = { backStack.popUnlessRoot() },
                )
            }
        },
    )
}
