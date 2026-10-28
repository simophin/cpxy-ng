package dev.fanchao.cpxy.rtp

import java.util.concurrent.atomic.AtomicLong

/** Guards synchronous native results against stop, revoke, destroy and newer connect requests. */
internal class ConnectionGeneration {
    private val current = AtomicLong()
    fun replace(): Long = current.incrementAndGet()
    fun isCurrent(ticket: Long): Boolean = current.get() == ticket
}
