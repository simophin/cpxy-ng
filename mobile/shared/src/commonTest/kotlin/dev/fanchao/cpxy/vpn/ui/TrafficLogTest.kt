package dev.fanchao.cpxy.vpn.ui

import dev.fanchao.cpxy.vpn.ConnectionEvent
import kotlin.test.Test
import kotlin.test.assertEquals

class TrafficLogTest {
    private fun event(port: Int) = ConnectionEvent(
        host = "1.2.3.4",
        port = port,
        outbound = "proxy",
        delayMillis = 1,
        timeMillis = 2,
        error = null,
        countryCode = "AU",
    )

    @Test
    fun keepsEntriesOldestFirst() {
        val log = TrafficLog()
        log.append(listOf(event(1), event(2)))
        log.append(listOf(event(3)))
        assertEquals(listOf(1, 2, 3), log.entries.map { it.event.port })
        assertEquals(listOf(0L, 1L, 2L), log.entries.map { it.id })
    }

    @Test
    fun dropsTheOldestBeyondTheLimit() {
        val size = event(0).estimatedBytes().toLong()
        val log = TrafficLog(maxBytes = size * 3)
        log.append((1..2).map(::event))
        log.append((3..5).map(::event))
        assertEquals(listOf(3, 4, 5), log.entries.map { it.event.port })

        log.append(listOf(event(6)))
        assertEquals(listOf(4, 5, 6), log.entries.map { it.event.port })
        // Ids keep counting, so the remaining rows keep their keys.
        assertEquals(listOf(3L, 4L, 5L), log.entries.map { it.id })
    }

    @Test
    fun flagsCountryCodes() {
        assertEquals("🇳🇿", flagEmoji("NZ"))
        assertEquals("🇨🇳", flagEmoji("CN"))
        assertEquals("🌐", flagEmoji(null))
        assertEquals("🌐", flagEmoji("nz"))
    }
}
