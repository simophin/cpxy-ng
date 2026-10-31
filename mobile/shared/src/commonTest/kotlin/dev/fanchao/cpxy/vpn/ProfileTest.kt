package dev.fanchao.cpxy.vpn

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
    fun oldProfilesKeepTheirIdentityCredentialsAndBackend() {
        val saved = Json.decodeFromString<StoredProfiles>("""{"selectedId":"old","profiles":[{"id":"old","name":"Old","server":"http://:key@example.com","dnsUpstream":["223.5.5.5"],"dnsAlternative":["8.8.8.8"]}]}""")
        assertEquals("old", saved.selected?.id)
        assertEquals("http://:key@example.com", saved.selected?.server)
        assertEquals(ProfileBackend.Cpxy, saved.selected?.backend)
        assertEquals(saved, Json.decodeFromString<StoredProfiles>(Json.encodeToString(saved)))
    }

    @Test
    fun externalSocksValidatesAndExportsWithoutCpxyCredentials() {
        val socks = profile.copy(backend = ProfileBackend.Socks5, socks5 = "[::1]:1080", server = "")
        assertTrue(socks.validationErrors().isEmpty())
        val config = Json.decodeFromString<EngineConfig>(socks.engineConfigJson(androidSocks = true))
        assertEquals("", config.server)
        assertEquals("[::1]:1080", config.socks5)
        assertTrue(config.androidSocks)
        assertEquals(socks, Json.decodeFromString<Profile>(Json.encodeToString(socks)))
        for (endpoint in listOf("https://host:1080", "user:pass@host:1080", "host:0", "host:65536", "::1:1080", "host")) {
            assertTrue(socks.copy(socks5 = endpoint).validationErrors().containsKey(ProfileField.Server), endpoint)
        }
        assertTrue(validSocksEndpoint("proxy.example.com:1080"))
    }

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

}
