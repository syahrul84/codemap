package com.example.demo.ui

import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.res.stringResource
import androidx.compose.material3.ModalBottomSheet
import androidx.compose.material3.Text
import com.example.demo.AppState
import com.example.demo.R

sealed interface Route {
    data class Detail(val id: Int) : Route
    data object Settings : Route
}

@Composable
fun AppTheme(content: @Composable () -> Unit) { content() }

@Composable
fun Root(state: AppState) {
    var tab by rememberSaveable { mutableIntStateOf(0) }
    val stack = remember { mutableStateListOf<Route>() }
    var showFilter by remember { mutableStateOf(false) }
    when (val top = stack.lastOrNull()) {
        is Route.Detail -> DetailScreen(state, top.id)
        Route.Settings -> SettingsScreen(state)
        else -> when (tab) {
            0 -> ItemsScreen(state) { stack.add(it) }
            else -> SettingsScreen(state)
        }
    }
    if (showFilter) ModalBottomSheet(onDismissRequest = { showFilter = false }) { FilterSheet() }
}

@Composable
fun ItemsScreen(state: AppState, navigate: (Route) -> Unit) {
    Text(stringResource(R.string.items_title))
    state.items.forEach { ItemRow(it.name) { navigate(Route.Detail(it.id)) } }
}

@Composable
private fun ItemRow(name: String, onClick: () -> Unit) { Text(name) }

@Composable
fun DetailScreen(state: AppState, id: Int) { Text("$id") }

@Composable
fun SettingsScreen(state: AppState) { Text("settings") }

@Composable
fun FilterSheet() { Text("filter") }

@Composable
fun UnusedScreen() { Text("nobody shows me") }
