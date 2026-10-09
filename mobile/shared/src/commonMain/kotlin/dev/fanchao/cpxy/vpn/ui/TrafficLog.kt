package dev.fanchao.cpxy.vpn.ui

import androidx.compose.runtime.mutableStateListOf
import dev.fanchao.cpxy.vpn.ConnectionEvent

/**
 * The connections shown on the traffic screen, oldest first. Once their estimated size passes
 * [maxBytes], the oldest are dropped.
 */
internal class TrafficLog(private val maxBytes: Long = MAX_BYTES) {
    class Entry(
        /** Unique within the log, to key the list rows. */
        val id: Long,
        val event: ConnectionEvent,
    ) {
        internal val estimatedBytes = event.estimatedBytes()
    }

    private val mutableEntries = mutableStateListOf<Entry>()
    val entries: List<Entry> get() = mutableEntries

    private var nextId = 0L
    private var bytes = 0L

    fun append(events: List<ConnectionEvent>) {
        for (event in events) {
            val entry = Entry(nextId++, event)
            mutableEntries += entry
            bytes += entry.estimatedBytes
        }

        var dropped = 0
        while (bytes > maxBytes && dropped < mutableEntries.size) {
            bytes -= mutableEntries[dropped++].estimatedBytes
        }
        if (dropped > 0) mutableEntries.removeRange(0, dropped)
    }

    companion object {
        const val MAX_BYTES = 5L * 1024 * 1024
    }
}

/** Roughly what an entry holds on the heap: the objects, the strings and its slot in the list. */
internal fun ConnectionEvent.estimatedBytes(): Int {
    val chars = host.length + outbound.length + (error?.length ?: 0) + (countryCode?.length ?: 0)
    return ENTRY_OVERHEAD_BYTES + 2 * chars
}

private const val ENTRY_OVERHEAD_BYTES = 160

/** The flag emoji of an ISO 3166-1 alpha-2 country code, or a globe when it is unknown. */
internal fun flagEmoji(countryCode: String?): String {
    if (countryCode == null || countryCode.length != 2 || !countryCode.all { it in 'A'..'Z' }) {
        return "🌐"
    }
    // Each letter maps to a regional indicator symbol, U+1F1E6 for A onwards; the pair renders as
    // the flag. They are outside the BMP, so each is a surrogate pair.
    return buildString {
        for (letter in countryCode) {
            append('\uD83C')
            append((0xDDE6 + (letter - 'A')).toChar())
        }
    }
}
