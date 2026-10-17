package dev.fanchao.cpxy.vpn

import java.io.File
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertNull

/** Looks up addresses in the data the Android app bundles. Gradle runs it from `mobile/shared`. */
class BundledCountryDataTest {
    private val lookup = CountryLookup(File("../androidApp/src/main/assets/country.dat").readBytes())

    @Test
    fun findsKnownCountries() {
        assertEquals("CN", lookup.countryOf("1.0.1.1"))
        assertEquals("AU", lookup.countryOf("1.0.0.1"))
        assertEquals("US", lookup.countryOf("8.8.8.8"))
        assertNull(lookup.countryOf("192.168.1.1"))
    }
}
