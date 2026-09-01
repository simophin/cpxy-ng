package dev.fanchao.cpxy.desktop

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

class DesktopTrayTest {
    @Test
    fun selectingDisabledProfileEnablesIt() {
        assertEquals("secondary", toggledProfileId("primary", "secondary"))
        assertEquals("primary", toggledProfileId(null, "primary"))
    }

    @Test
    fun selectingEnabledProfileDisablesIt() {
        assertNull(toggledProfileId("primary", "primary"))
    }
}
