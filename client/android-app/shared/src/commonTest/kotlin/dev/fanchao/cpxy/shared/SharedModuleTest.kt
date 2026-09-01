package dev.fanchao.cpxy.shared

import kotlin.test.Test
import kotlin.test.assertEquals

class SharedModuleTest {
    @Test
    fun describesInitialSharedTargets() {
        assertEquals("Cpxy", SharedModule.info.name)
        assertEquals(setOf("Android", "Desktop"), SharedModule.info.supportedPlatforms)
    }
}
