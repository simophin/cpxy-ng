package dev.fanchao.cpxy.vpn

/** The bytes [CountryLookup] searches, read in place, such as a memory-mapped file. */
interface CountryData {
    val size: Int
    operator fun get(index: Int): Byte
}

/**
 * Finds the country of IPv4 addresses in [data]: non-overlapping ranges sorted by start, each as
 * a big-endian 4 bytes start, 4 bytes inclusive end and 2 ASCII bytes ISO 3166-1 alpha-2 code.
 * The Android app bundles them as the `country.dat` asset (see `geoip-data/SOURCE.md`).
 */
class CountryLookup(private val data: CountryData) {
    constructor(data: ByteArray) : this(object : CountryData {
        override val size get() = data.size
        override fun get(index: Int) = data[index]
    })

    init {
        require(data.size % ENTRY_SIZE == 0) { "Invalid data length ${data.size}" }
    }

    /** The country code of a dotted IPv4 address, or null when it is unknown or not IPv4. */
    fun countryOf(ip: String): String? {
        val address = parseIpv4(ip) ?: return null
        // The first range starting after the address; only the one before it can hold it.
        var low = 0
        var high = data.size / ENTRY_SIZE
        while (low < high) {
            val mid = (low + high) ushr 1
            if (readUInt(mid * ENTRY_SIZE) <= address) low = mid + 1 else high = mid
        }
        if (low == 0) return null
        val entry = (low - 1) * ENTRY_SIZE
        if (address > readUInt(entry + 4)) return null
        return charArrayOf(data[entry + 8].toInt().toChar(), data[entry + 9].toInt().toChar())
            .concatToString()
    }

    private fun readUInt(offset: Int): UInt =
        (0 until 4).fold(0u) { value, i -> (value shl 8) or (data[offset + i].toUInt() and 0xFFu) }

    private companion object {
        const val ENTRY_SIZE = 10

        fun parseIpv4(ip: String): UInt? {
            val parts = ip.split('.')
            if (parts.size != 4) return null
            return parts.fold(0u) { value, part ->
                if (part.isEmpty() || part.length > 3 || !part.all { it in '0'..'9' }) return null
                val octet = part.toUInt()
                if (octet > 255u) return null
                (value shl 8) or octet
            }
        }
    }
}
