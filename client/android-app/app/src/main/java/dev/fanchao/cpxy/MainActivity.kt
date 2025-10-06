package dev.fanchao.cpxy

import android.Manifest
import android.content.pm.PackageManager
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview
import androidx.core.content.ContextCompat
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.toRoute
import dev.fanchao.cpxy.App.Companion.appInstance
import dev.fanchao.cpxy.ui.EditProfileRoute
import dev.fanchao.cpxy.ui.EditProfileScreen
import dev.fanchao.cpxy.ui.HomeRoute
import dev.fanchao.cpxy.ui.HomeScreen
import dev.fanchao.cpxy.ui.theme.CpxyTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        if (ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
        
        enableEdgeToEdge()
        setContent {
            val navController = rememberNavController()

            CpxyTheme {
                NavHost(navController = navController, startDestination = HomeRoute) {
                    composable<HomeRoute> {
                        HomeScreen(
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
                            configurationRepository = appInstance.configurationRepository,
                        )
                    }
                }
            }
        }
    }
}

@Composable
fun Greeting(name: String, modifier: Modifier = Modifier) {
    Text(
        text = "Hello $name!",
        modifier = modifier
    )
}

@Preview(showBackground = true)
@Composable
fun GreetingPreview() {
    CpxyTheme {
        Greeting("Android")
    }
}