package dev.fanchao.cpxy.ui.events

import java.text.NumberFormat
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import java.time.format.FormatStyle
import kotlin.time.Instant
import kotlin.time.toJavaInstant

class AndroidEventTextFormatter : EventTextFormatter {
    override fun formatDelayMillis(delayMillis: Long): String =
        NumberFormat.getNumberInstance().format(delayMillis) + "ms"

    override fun formatTime(instant: Instant): String =
        instant.toJavaInstant()
            .atZone(ZoneId.systemDefault())
            .format(DateTimeFormatter.ofLocalizedTime(FormatStyle.SHORT))
}
