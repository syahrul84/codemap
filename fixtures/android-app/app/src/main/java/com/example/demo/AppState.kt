package com.example.demo

/** Shared by every screen. */
class AppState {
    val items = mutableListOf<Item>()
    var open by mutableStateOf(false); private set
    fun refresh() { open = true }
}

data class Item(val id: Int, val name: String)

enum class Filter(val code: String) { ALL("all"), DONE("done") }
