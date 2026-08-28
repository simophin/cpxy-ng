package dev.fanchao.cpxy.ui.events

import java.util.Locale
import java.util.TimeZone
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.time.Instant

class JvmEventTextFormatterTest {
    @Test
    fun formatsEventTextUsingPlatformLocaleAndTimeZone() {
        val originalLocale = Locale.getDefault()
        val originalTimeZone = TimeZone.getDefault()
        try {
            Locale.setDefault(Locale.US)
            TimeZone.setDefault(TimeZone.getTimeZone("UTC"))

            val formatter = JvmEventTextFormatter()

            assertEquals("12,345ms", formatter.formatDelayMillis(12_345))
            assertEquals(
                "1:05 PM",
                formatter.formatTime(Instant.parse("2026-08-28T13:05:00Z"))
                    .replace('\u202f', ' '),
            )
        } finally {
            Locale.setDefault(originalLocale)
            TimeZone.setDefault(originalTimeZone)
        }
    }
}
