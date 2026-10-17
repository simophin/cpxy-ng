package dev.fanchao.cpxy.vpn

import dev.fanchao.cpxy.vpn.ui.formatBytes
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
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
                    "mtu": 1500,
                    "ipv6": "full"
                }
                """
            ) as JsonObject,
            json,
        )
    }

    @Test
    fun engineConfigIncludesOnlyEnabledProxyPorts() {
        val settings = AppSettings(httpProxyEnabled = true, httpProxyPort = 8888)
        val config = Json.decodeFromString<EngineConfig>(profile.engineConfigJson(settings))
        assertEquals(8888, config.httpProxyPort)
        assertEquals(null, config.socksProxyPort)
        val socks = Json.decodeFromString<EngineConfig>(profile.engineConfigJson(settings.copy(httpProxyEnabled = false, socksProxyEnabled = true)))
        assertEquals(null, socks.httpProxyPort)
        assertEquals(1080, socks.socksProxyPort)
    }

    @Test
    fun engineConfigIncludesProxyRoutes() {
        val routes = listOf("223.5.5.5/32", "10.0.0.0/8")
        val config = Json.decodeFromString<EngineConfig>(profile.engineConfigJson(AppSettings(proxyRoutes = routes)))
        assertEquals(routes, config.proxyRoutes)
    }

    @Test
    fun engineConfigIncludesSearchDomainsInOrder() {
        val domains = listOf("home.arpa", "corp.example")
        val config = Json.decodeFromString<EngineConfig>(profile.engineConfigJson(AppSettings(dnsSearchDomains = domains)))
        assertEquals(domains, config.dnsSearchDomains)
    }

    @Test
    fun engineConfigIncludesTheIpv6Mode() {
        for ((mode, name) in listOf(Ipv6Mode.Full to "full", Ipv6Mode.Direct to "direct", Ipv6Mode.Off to "off")) {
            val json = Json.parseToJsonElement(profile.copy(ipv6 = mode).engineConfigJson()).jsonObject
            assertEquals(name, json["ipv6"]?.jsonPrimitive?.content)
        }
    }

    @Test
    fun profilesSavedBeforeIpv6UseFullIpv6() {
        val saved = Json.decodeFromString<Profile>(
            "{\"id\":\"1\",\"name\":\"Home\",\"server\":\"https://:k@h\",\"dnsUpstream\":[],\"dnsAlternative\":[]}",
        )
        assertEquals(Ipv6Mode.Full, saved.ipv6)
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
