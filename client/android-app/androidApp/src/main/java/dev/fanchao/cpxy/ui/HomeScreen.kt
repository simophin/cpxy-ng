package dev.fanchao.cpxy.ui

import androidx.compose.animation.Crossfade
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.List
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import dev.fanchao.cpxy.Profile
import dev.fanchao.cpxy.R
import kotlinx.serialization.Serializable

@Serializable
data object HomeRoute

private enum class NavItem(val icon: ImageVector, val label: String) {
    Profiles(Icons.Default.Home, "Home"),
    EventList(Icons.AutoMirrored.Default.List, "Events"),
    Settings(Icons.Default.Settings, "Settings"),
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun HomeScreen(
    navigateToEditScreen: (Profile) -> Unit,
    navigateToNewConfigScreen: () -> Unit,
) {
    var selectedNavItem by remember { mutableStateOf(NavItem.Profiles) }
    val snackbarHostState = remember { SnackbarHostState() }
    val context = LocalContext.current

    Scaffold(
        snackbarHost = { SnackbarHost(hostState = snackbarHostState) },
        topBar = {
            TopAppBar(
                title = { Text(context.getString(R.string.app_name)) },
            )
        },
        bottomBar = {
            NavigationBar {
                for (item in NavItem.entries) {
                    NavigationBarItem(
                        selected = selectedNavItem == item,
                        onClick = { selectedNavItem = item },
                        icon = {
                            Icon(imageVector = item.icon, contentDescription = null)
                        },
                        label = {
                            Text(text = item.label)
                        }
                    )
                }
            }
        },
        floatingActionButton = {
            Crossfade(selectedNavItem) { item ->
                when (item) {
                    NavItem.Profiles -> {
                        FloatingActionButton(onClick = navigateToNewConfigScreen) {
                            Icon(Icons.Default.Add, contentDescription = null)
                        }
                    }

                    NavItem.EventList, NavItem.Settings -> {}
                }
            }
        }
    ) { padding ->
        Surface {
            Crossfade(selectedNavItem) { state ->
                when (state) {
                    NavItem.Profiles -> ProfileList(
                        modifier = Modifier.padding(padding),
                        navigateToEditScreen = navigateToEditScreen,
                    )

                    NavItem.EventList -> EventViewer(modifier = Modifier.padding(padding))
                    NavItem.Settings -> Settings(
                        modifier = Modifier.padding(padding),
                        snackbarHostState = snackbarHostState
                    )
                }
            }

        }
    }
}