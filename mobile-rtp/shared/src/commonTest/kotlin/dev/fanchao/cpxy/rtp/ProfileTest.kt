package dev.fanchao.cpxy.rtp

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class ProfileTest {
    private val profile = Profile(
        id = "id",
        name = "Home",
        endpoint = " vpn.example.com:30000 ",
        dnsUpstream = listOf("223.5.5.5", "tls://dns.alidns.com"),
    )

    @Test
    fun engineProfileHasOnlyTheEngineFields() {
        assertEquals(
            """{"endpoint":"vpn.example.com:30000",""" +
                """"dns_upstream":["223.5.5.5","tls://dns.alidns.com"],"dns_alternative":["8.8.8.8","1.1.1.1"]}""",
            profile.engineProfileJson(),
        )
    }

    @Test
    fun validProfileHasNoErrors() {
        assertEquals(emptyMap(), profile.validationErrors())
    }

    @Test
    fun reportsEachInvalidField() {
        val errors = profile.copy(
            name = " ",
            endpoint = "",
            dnsUpstream = emptyList(),
            dnsAlternative = listOf("dns.google"),
        ).validationErrors()
        assertEquals(
            setOf(
                ProfileField.Name,
                ProfileField.Endpoint,
                ProfileField.DnsUpstream,
                ProfileField.DnsAlternative,
            ),
            errors.keys,
        )
    }

    @Test
    fun ipv4CheckRejectsLeadingZerosAndNames() {
        assertTrue(isIpv4("10.88.0.2"))
        for (bad in listOf("10.88.0", "010.88.0.2", "10.88.0.2.1", "a.b.c.d", "", "1.1.1.1 ")) {
            assertTrue(!isIpv4(bad), bad)
        }
    }

    @Test
    fun serverListSkipsBlankLines() {
        assertEquals(listOf("8.8.8.8", "1.1.1.1"), parseServerList(" 8.8.8.8\n\n 1.1.1.1 \n"))
    }
}
