package dev.fanchao.cpxy.vpn.ui

import dev.fanchao.cpxy.vpn.ConnectionEvent
import dev.fanchao.cpxy.vpn.ConnectionPage
import dev.fanchao.cpxy.vpn.ConnectionRecord
import dev.fanchao.cpxy.vpn.Profile
import dev.fanchao.cpxy.vpn.Traffic
import dev.fanchao.cpxy.vpn.VpnController
import dev.fanchao.cpxy.vpn.VpnState
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.test.runTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/** Keeps the newest [capacity] connections, like the engine. */
private class FakeController(private val capacity: Int = 100) : VpnController {
    override val state: StateFlow<VpnState> = MutableStateFlow(VpnState.Disconnected)
    override val traffic: StateFlow<Traffic> = MutableStateFlow(Traffic())

    private val records = ArrayDeque<ConnectionRecord>()
    private var nextSeq = 0L

    fun add(count: Int) = repeat(count) {
        if (records.size == capacity) records.removeFirst()
        records += ConnectionRecord(nextSeq++, event)
    }

    private fun page(records: List<ConnectionRecord>) =
        ConnectionPage(records, this.records.firstOrNull()?.seq ?: nextSeq)

    override suspend fun connectionsSince(since: Long, limit: Int) =
        page(records.filter { it.seq >= since }.take(limit))

    override suspend fun connectionsBefore(before: Long?, limit: Int) =
        page(records.filter { before == null || it.seq < before }.takeLast(limit))

    override suspend fun connect(profile: Profile) = Unit
    override fun disconnect() = Unit

    private companion object {
        val event = ConnectionEvent(
            host = "1.2.3.4",
            port = 443,
            outbound = "proxy",
            delayMillis = 1,
            timeMillis = 2,
            error = null,
            countryCode = "AU",
        )
    }
}

class TrafficLogTest {
    private val TrafficLog.seqs get() = entries.map { it.seq }

    @Test
    fun startsWithTheNewestPage() = runTest {
        val controller = FakeController().apply { add(25) }
        val log = TrafficLog(controller, pageSize = 10)
        log.loadNewer()
        assertEquals((15L..24L).toList(), log.seqs)
        assertTrue(log.hasOlder)
    }

    @Test
    fun appendsEveryNewerPage() = runTest {
        val controller = FakeController().apply { add(5) }
        val log = TrafficLog(controller, pageSize = 10)
        log.loadNewer()
        controller.add(25)
        log.loadNewer()
        assertEquals((0L..29L).toList(), log.seqs)
        assertFalse(log.hasOlder)
    }

    @Test
    fun reloadsTheNewestWhenConnectionsWereDroppedInBetween() = runTest {
        val controller = FakeController(capacity = 20).apply { add(5) }
        val log = TrafficLog(controller, pageSize = 10)
        log.loadNewer()
        controller.add(30)
        log.loadNewer()
        assertEquals((25L..34L).toList(), log.seqs)
        assertTrue(log.hasOlder)
    }

    @Test
    fun prependsOlderPagesUntilTheOldest() = runTest {
        val controller = FakeController().apply { add(25) }
        val log = TrafficLog(controller, pageSize = 10)
        log.loadNewer()
        log.loadOlder()
        assertEquals((5L..24L).toList(), log.seqs)
        assertTrue(log.hasOlder)
        log.loadOlder()
        assertEquals((0L..24L).toList(), log.seqs)
        assertFalse(log.hasOlder)
    }

    @Test
    fun dropsFromTheOtherEndBeyondTheLimit() = runTest {
        val controller = FakeController().apply { add(30) }
        val log = TrafficLog(controller, pageSize = 10, maxEntries = 15)
        log.loadNewer()
        log.loadOlder()
        // The newest are dropped while loading older ones…
        assertEquals((10L..24L).toList(), log.seqs)
        controller.add(10)
        log.loadNewer()
        // …and the oldest while loading newer ones.
        assertEquals((25L..39L).toList(), log.seqs)
        assertTrue(log.hasOlder)
    }

    @Test
    fun flagsCountryCodes() {
        assertEquals("🇳🇿", flagEmoji("NZ"))
        assertEquals("🇨🇳", flagEmoji("CN"))
        assertEquals("🌐", flagEmoji(null))
        assertEquals("🌐", flagEmoji("nz"))
    }
}
