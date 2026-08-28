package dev.fanchao.cpxy

import dev.fanchao.cpxy.app.ClientConfig
import dev.fanchao.cpxy.app.Profile
import kotlinx.serialization.json.Json
import org.junit.Assert.assertEquals
import org.junit.Test

class ConfigSerializationTest {
    private val json = Json {
        ignoreUnknownKeys = true
        isLenient = true
    }

    @Test
    fun roundTripPreservesCurrentConfiguration() {
        val profile = Profile(
            id = "primary",
            name = "Primary",
            mainServerUrl = "wss://main.example.test",
            aiServerUrl = "wss://ai.example.test",
            tailscaleServerUrl = null,
        )
        val config = ClientConfig(
            profiles = listOf(profile),
            enabledProfileId = profile.id,
            httpProxyPort = 8_080u,
            socks5ProxyPort = 1_080u,
            apiServerPort = 3_011u,
            dnsServer = "1.1.1.1",
        )

        val decoded = json.decodeFromString<ClientConfig>(json.encodeToString(config))

        assertEquals(config, decoded)
        assertEquals(profile, decoded.enabledProfile)
    }

    @Test
    fun legacyConfigurationUsesDefaultsAndIgnoresUnknownFields() {
        val legacyJson = """
            {
              "profiles": [],
              "enabledProfileId": null,
              "httpProxyPort": 8080,
              "socks5ProxyPort": 1080,
              "futureSetting": "preserved compatibility"
            }
        """.trimIndent()

        val decoded = json.decodeFromString<ClientConfig>(legacyJson)

        assertEquals(3_010u.toUShort(), decoded.apiServerPort)
        assertEquals("223.5.5.5", decoded.dnsServer)
        assertEquals(8_080u.toUShort(), decoded.httpProxyPort)
        assertEquals(1_080u.toUShort(), decoded.socks5ProxyPort)
    }
}
