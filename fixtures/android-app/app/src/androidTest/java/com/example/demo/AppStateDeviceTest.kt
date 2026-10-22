package com.example.demo

import org.junit.Test

class AppStateDeviceTest {
    @Test fun startsClosed() { val s = AppState(); s.refresh() }
}
