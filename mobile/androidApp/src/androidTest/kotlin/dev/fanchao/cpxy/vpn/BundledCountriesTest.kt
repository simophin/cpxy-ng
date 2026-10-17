package dev.fanchao.cpxy.vpn

import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class BundledCountriesTest {
    private val countries = InstrumentationRegistry.getInstrumentation().targetContext.bundledCountries()

    @Test
    fun findsKnownCountriesInTheMappedAsset() {
        assertEquals("CN", countries.countryOf("1.0.1.1"))
        assertEquals("AU", countries.countryOf("1.0.0.1"))
        assertEquals("US", countries.countryOf("8.8.8.8"))
        assertNull(countries.countryOf("192.168.1.1"))
    }
}
