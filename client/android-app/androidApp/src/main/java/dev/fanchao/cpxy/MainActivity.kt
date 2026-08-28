package dev.fanchao.cpxy

import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.remember
import androidx.core.content.ContextCompat
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.toRoute
import dev.fanchao.cpxy.ui.EditProfileRoute
import dev.fanchao.cpxy.ui.EditProfileScreen
import dev.fanchao.cpxy.ui.HomeRoute
import dev.fanchao.cpxy.ui.HomeScreen
import dev.fanchao.cpxy.ui.events.AndroidEventTextFormatter
import dev.fanchao.cpxy.ui.theme.AndroidCpxyTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        if (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
        
        enableEdgeToEdge()
        val controller = appGraph.appController
        setContent {
            val navController = rememberNavController()
            val eventTextFormatter = remember { AndroidEventTextFormatter() }

            AndroidCpxyTheme {
                NavHost(navController = navController, startDestination = HomeRoute) {
                    composable<HomeRoute> {
                        HomeScreen(
                            controller = controller,
                            eventTextFormatter = eventTextFormatter,
                            navigateToEditScreen = {
                                navController.navigate(EditProfileRoute(it.id))
                            },
                            navigateToNewConfigScreen = {
                                navController.navigate(EditProfileRoute(null))
                            },
                        )
                    }

                    composable<EditProfileRoute> {
                        val route: EditProfileRoute = it.toRoute()
                        EditProfileScreen(
                            profileId = route.id,
                            onDone = navController::popBackStack,
                            configurationRepository = controller.configRepository,
                        )
                    }
                }
            }
        }
    }
}
