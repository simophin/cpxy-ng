package dev.fanchao.cpxy.vpn

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlin.test.assertNull

class CountryLookupTest {
    private fun entry(from: UInt, to: UInt, country: String): ByteArray {
        val bytes = ByteArray(10)
        for (i in 0 until 4) {
            bytes[i] = (from shr (24 - 8 * i)).toByte()
            bytes[4 + i] = (to shr (24 - 8 * i)).toByte()
        }
        country.encodeToByteArray().copyInto(bytes, 8)
        return bytes
    }

    // The last range is above 128.0.0.0, where signed comparison would sort it first.
    private val lookup = CountryLookup(
        entry(0x01000000u, 0x010000FFu, "AU") +
            entry(0x01000100u, 0x0100FFFFu, "CN") +
            entry(0x08080800u, 0x08080808u, "US") +
            entry(0xC0000000u, 0xFFFFFFFFu, "NZ")
    )

    @Test
    fun findsTheCountryOfAddressesInRanges() {
        assertEquals("AU", lookup.countryOf("1.0.0.0"))
        assertEquals("AU", lookup.countryOf("1.0.0.255"))
        assertEquals("CN", lookup.countryOf("1.0.1.0"))
        assertEquals("US", lookup.countryOf("8.8.8.8"))
        assertEquals("NZ", lookup.countryOf("192.0.0.0"))
        assertEquals("NZ", lookup.countryOf("255.255.255.255"))
    }

    @Test
    fun returnsNullOutsideRanges() {
        assertNull(lookup.countryOf("0.255.255.255"))
        assertNull(lookup.countryOf("1.1.0.0"))
        assertNull(lookup.countryOf("8.8.8.9"))
        assertNull(lookup.countryOf("191.255.255.255"))
        assertNull(CountryLookup(ByteArray(0)).countryOf("1.0.0.0"))
    }

    @Test
    fun returnsNullForWhatIsNotAnIpv4Address() {
        for (ip in listOf("", "1.0.0", "1.0.0.0.0", "1.0.0.256", "1.0.0.-1", "1.0.0.+1", "::1", "a.b.c.d")) {
            assertNull(lookup.countryOf(ip), ip)
        }
    }

    @Test
    fun rejectsTruncatedData() {
        assertFailsWith<IllegalArgumentException> { CountryLookup(ByteArray(11)) }
    }
}
