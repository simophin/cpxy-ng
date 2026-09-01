package dev.fanchao.cpxy.ui.events

import kotlin.time.Instant

interface EventTextFormatter {
    fun formatDelayMillis(delayMillis: Long): String

    fun formatTime(instant: Instant): String
}
