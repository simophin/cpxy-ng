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
import dev.fanchao.cpxy.ui.EditConfigRoute
import dev.fanchao.cpxy.ui.EditConfigScreen
import dev.fanchao.cpxy.ui.ServerListRoute
import dev.fanchao.cpxy.ui.ServerListScreen
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
                NavHost(navController = navController, startDestination = ServerListRoute) {
                    composable<ServerListRoute> {
                        ServerListScreen(
                            configurationRepository = appInstance.configurationRepository,
                            clientInstanceManager = appInstance.clientInstanceManager,
                            navigateToEditScreen = {
                                navController.navigate(EditConfigRoute(it.id))
                            },
                            navigateToNewConfigScreen = {
                                navController.navigate(EditConfigRoute(null))
                            }
                        )
                    }

                    composable<EditConfigRoute> {
                        val route: EditConfigRoute = it.toRoute()
                        EditConfigScreen(
                            configId = route.id,
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