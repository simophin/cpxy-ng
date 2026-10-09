package dev.fanchao.cpxy.vpn

import dev.fanchao.cpxy.vpn.ui.formatBytes
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class ProfileTest {
    private val profile = Profile(
        id = "1",
        name = "Home",
        server = " https://:key@example.com ",
        dnsUpstream = listOf("223.5.5.5", "tcp://119.29.29.29"),
        dnsAlternative = listOf("https://dns.google/dns-query?ip=8.8.8.8"),
    )

    @Test
    fun engineConfigUsesTheEngineFieldNames() {
        val json = Json.parseToJsonElement(profile.engineConfigJson()).jsonObject
        assertEquals(
            Json.parseToJsonElement(
                """
                {
                    "server": "https://:key@example.com",
                    "dns_upstream": ["223.5.5.5", "tcp://119.29.29.29"],
                    "dns_alternative": ["https://dns.google/dns-query?ip=8.8.8.8"],
                    "mtu": 1500
                }
                """
            ) as JsonObject,
            json,
        )
    }

    @Test
    fun validProfileHasNoErrors() {
        assertTrue(profile.validationErrors().isEmpty())
    }

    @Test
    fun reportsEveryMissingField() {
        val empty = Profile("1", " ", "", emptyList(), emptyList())
        assertEquals(ProfileField.entries.toSet(), empty.validationErrors().keys)
    }

    @Test
    fun parsesOneServerPerLine() {
        assertEquals(
            listOf("1.1.1.1", "tls://8.8.8.8"),
            parseServerList(" 1.1.1.1 \n\n  tls://8.8.8.8\n"),
        )
    }

    @Test
    fun formatsBytes() {
        assertEquals("512 B", formatBytes(512))
        assertEquals("1.5 KB", formatBytes(1536))
        assertEquals("3.0 MB", formatBytes(3L * 1024 * 1024))
    }
}
