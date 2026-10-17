package dev.fanchao.cpxy.vpn

import android.content.Context
import java.io.FileInputStream
import java.nio.ByteBuffer
import java.nio.channels.FileChannel

/**
 * Looks up countries in the bundled `country.dat` asset, memory-mapped so that only the pages a
 * search touches are read. The asset is stored uncompressed (see `noCompress` in the build) so
 * that it can be mapped; the mapping outlives the descriptor.
 */
fun Context.bundledCountries(): CountryLookup {
    val buffer = assets.openFd("country.dat").use { fd ->
        FileInputStream(fd.fileDescriptor).channel.use {
            it.map(FileChannel.MapMode.READ_ONLY, fd.startOffset, fd.length)
        }
    }
    return CountryLookup(BufferCountryData(buffer))
}

/** Reads with absolute gets only, so it is safe from any thread. */
private class BufferCountryData(private val buffer: ByteBuffer) : CountryData {
    override val size get() = buffer.capacity()
    override fun get(index: Int) = buffer.get(index)
}
