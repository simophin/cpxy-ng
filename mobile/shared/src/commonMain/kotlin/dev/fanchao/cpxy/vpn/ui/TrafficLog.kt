package dev.fanchao.cpxy.vpn.ui

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateListOf
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import dev.fanchao.cpxy.vpn.ConnectionPage
import dev.fanchao.cpxy.vpn.ConnectionRecord
import dev.fanchao.cpxy.vpn.VpnController
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/**
 * A window onto the engine's recent connections, oldest first, paged in from either end. Holds
 * at most [maxEntries], dropping from the end away from the one being loaded.
 */
internal class TrafficLog(
    private val controller: VpnController,
    private val pageSize: Int = PAGE_SIZE,
    private val maxEntries: Int = MAX_ENTRIES,
) {
    private val mutableEntries = mutableStateListOf<ConnectionRecord>()
    val entries: List<ConnectionRecord> get() = mutableEntries

    /** Whether the engine keeps connections older than the [entries]. */
    var hasOlder by mutableStateOf(false)
        private set

    // Loads from both ends may overlap, and each reads the entries across a query.
    private val mutex = Mutex()

    /** Loads the connections after the newest entry, or the newest page when there are none. */
    suspend fun loadNewer() = mutex.withLock {
        while (true) {
            val since = (mutableEntries.lastOrNull() ?: return@withLock replaceWithLatest()).seq + 1
            val page = controller.connectionsSince(since, pageSize)
            // Connections were dropped in between, so the entries would have a hole.
            if (page.oldestSeq > since) return@withLock replaceWithLatest()
            mutableEntries += page.records
            val excess = mutableEntries.size - maxEntries
            if (excess > 0) {
                mutableEntries.removeRange(0, excess)
                hasOlder = true
            }
            if (page.records.size < pageSize) return@withLock
        }
    }

    /** Loads a page of connections before the oldest entry. */
    suspend fun loadOlder() = mutex.withLock {
        val first = mutableEntries.firstOrNull() ?: return@withLock
        val page = controller.connectionsBefore(first.seq, pageSize)
        mutableEntries.addAll(0, page.records)
        hasOlder = page.hasOlder()
        if (mutableEntries.size > maxEntries) {
            mutableEntries.removeRange(maxEntries, mutableEntries.size)
        }
    }

    private suspend fun replaceWithLatest() {
        val page = controller.connectionsBefore(null, pageSize)
        mutableEntries.clear()
        mutableEntries += page.records
        hasOlder = page.hasOlder()
    }

    companion object {
        const val PAGE_SIZE = 200
        const val MAX_ENTRIES = 10_000
    }
}

private fun ConnectionPage.hasOlder() = records.firstOrNull()?.let { it.seq > oldestSeq } ?: false

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
