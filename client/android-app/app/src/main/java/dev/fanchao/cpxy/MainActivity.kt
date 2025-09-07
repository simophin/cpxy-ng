package dev.fanchao.cpxy

import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.rememberNavController
import androidx.navigation.toRoute
import dev.fanchao.cpxy.ui.EditConfigRoute
import dev.fanchao.cpxy.ui.EditConfigScreen
import dev.fanchao.cpxy.ui.ServerListRoute
import dev.fanchao.cpxy.ui.ServerListScreen
import dev.fanchao.cpxy.ui.theme.CpxyTheme

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        
        enableEdgeToEdge()
        setContent {
            val navController = rememberNavController()

            CpxyTheme {
                NavHost(navController = navController, startDestination = ServerListRoute) {
                    composable<ServerListRoute> {
                        ServerListScreen(
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