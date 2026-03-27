package com.zremote.ui.navigation

import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Computer
import androidx.compose.material.icons.filled.Loop
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.SmartToy
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.navigation.NavDestination.Companion.hasRoute
import androidx.navigation.NavHostController
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import androidx.navigation.toRoute
import com.zremote.ui.screens.hosts.HostListScreen
import com.zremote.ui.screens.loops.LoopDetailScreen
import com.zremote.ui.screens.loops.LoopListScreen
import com.zremote.ui.screens.sessions.SessionListScreen
import com.zremote.ui.screens.settings.SettingsScreen
import com.zremote.ui.screens.tasks.TaskListScreen
import com.zremote.ui.screens.terminal.TerminalScreen
import kotlinx.serialization.Serializable

// Route definitions
@Serializable object HostsRoute
@Serializable object LoopsRoute
@Serializable object TasksRoute
@Serializable object SettingsRoute
@Serializable data class SessionsRoute(val hostId: String)
@Serializable data class LoopDetailRoute(val loopId: String)
@Serializable data class TerminalRoute(val sessionId: String)

data class BottomNavItem(
    val label: String,
    val icon: ImageVector,
    val route: Any,
)

val bottomNavItems = listOf(
    BottomNavItem("Hosts", Icons.Default.Computer, HostsRoute),
    BottomNavItem("Loops", Icons.Default.Loop, LoopsRoute),
    BottomNavItem("Tasks", Icons.Default.SmartToy, TasksRoute),
    BottomNavItem("Settings", Icons.Default.Settings, SettingsRoute),
)

@Composable
fun ZRemoteNavHost(navController: NavHostController = rememberNavController()) {
    val navBackStackEntry by navController.currentBackStackEntryAsState()
    val currentDestination = navBackStackEntry?.destination

    Scaffold(
        bottomBar = {
            NavigationBar {
                bottomNavItems.forEach { item ->
                    NavigationBarItem(
                        icon = { Icon(item.icon, contentDescription = item.label) },
                        label = { Text(item.label) },
                        selected = currentDestination?.hasRoute(item.route::class) == true,
                        onClick = {
                            navController.navigate(item.route) {
                                popUpTo(HostsRoute) { saveState = true }
                                launchSingleTop = true
                                restoreState = true
                            }
                        },
                    )
                }
            }
        },
    ) { innerPadding ->
        NavHost(
            navController = navController,
            startDestination = HostsRoute,
            modifier = Modifier.padding(innerPadding),
        ) {
            composable<HostsRoute> {
                HostListScreen(
                    onHostClick = { hostId ->
                        navController.navigate(SessionsRoute(hostId))
                    },
                )
            }
            composable<SessionsRoute> { backStackEntry ->
                val route = backStackEntry.toRoute<SessionsRoute>()
                SessionListScreen(
                    hostId = route.hostId,
                    onSessionClick = { sessionId ->
                        navController.navigate(TerminalRoute(sessionId))
                    },
                )
            }
            composable<LoopsRoute> {
                LoopListScreen(
                    onLoopClick = { loopId ->
                        navController.navigate(LoopDetailRoute(loopId))
                    },
                )
            }
            composable<LoopDetailRoute> { backStackEntry ->
                val route = backStackEntry.toRoute<LoopDetailRoute>()
                LoopDetailScreen(loopId = route.loopId)
            }
            composable<TasksRoute> {
                TaskListScreen()
            }
            composable<TerminalRoute> { backStackEntry ->
                val route = backStackEntry.toRoute<TerminalRoute>()
                TerminalScreen(sessionId = route.sessionId)
            }
            composable<SettingsRoute> {
                SettingsScreen()
            }
        }
    }
}
