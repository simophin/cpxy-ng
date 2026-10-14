package dev.fanchao.cpxy.vpn

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.test.runTest
import okio.FileSystem
import kotlin.random.Random
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith
import kotlinx.serialization.json.Json

class SettingsTest {
    private val directory = FileSystem.SYSTEM_TEMPORARY_DIRECTORY / "cpxy-vpn-test-${Random.nextLong().toULong()}"
    private val scope = CoroutineScope(Dispatchers.Default + Job())
    private val repository = SettingsRepository.create(directory / "settings.preferences_pb", scope)

    @AfterTest
    fun cleanUp() {
        scope.cancel()
        FileSystem.SYSTEM.deleteRecursively(directory)
    }

    @Test
    fun startsWithAllApps() = runTest {
        assertEquals(AppSettings(), repository.settings.first())
    }

    @Test
    fun keepsEachFiltersApps() = runTest {
        repository.update { it.copy(appFilter = AppFilter.OnlySelected).withApps(AppFilter.OnlySelected, setOf("a")) }
        repository.update { it.copy(appFilter = AppFilter.ExceptSelected).withApps(AppFilter.ExceptSelected, setOf("b")) }

        val settings = repository.settings.first()
        assertEquals(AppFilter.ExceptSelected, settings.appFilter)
        assertEquals(setOf("a"), settings.appsFor(AppFilter.OnlySelected))
        assertEquals(setOf("b"), settings.appsFor(AppFilter.ExceptSelected))
    }

    @Test
    fun allAppsLeavesOutOnlyThisApp() {
        val settings = AppSettings(onlyApps = setOf("a"), exceptApps = setOf("b"))
        assertEquals(AppRouting.Except(setOf(OWN)), settings.routing(OWN))
    }

    @Test
    fun onlySelectedNeverIncludesThisApp() {
        val settings = AppSettings(AppFilter.OnlySelected, onlyApps = setOf("a", OWN))
        assertEquals(AppRouting.Only(setOf("a")), settings.routing(OWN))
    }

    @Test
    fun onlySelectedWithNoAppsCarriesAllOthers() {
        val settings = AppSettings(AppFilter.OnlySelected, onlyApps = setOf(OWN))
        assertEquals(AppRouting.Except(setOf(OWN)), settings.routing(OWN))
    }

    @Test
    fun exceptSelectedAlsoLeavesOutThisApp() {
        val settings = AppSettings(AppFilter.ExceptSelected, exceptApps = setOf("b"))
        assertEquals(AppRouting.Except(setOf("b", OWN)), settings.routing(OWN))
    }

    @Test
    fun proxySettingsPersistIndependently() = runTest {
        repository.update { it.copy(httpProxyEnabled = true, httpProxyPort = 8888, socksProxyEnabled = true, socksProxyPort = 1081) }
        repository.update { it.copy(httpProxyEnabled = false) }
        val settings = repository.settings.first()
        assertEquals(false, settings.httpProxyEnabled)
        assertEquals(8888, settings.httpProxyPort)
        assertEquals(true, settings.socksProxyEnabled)
        assertEquals(1081, settings.socksProxyPort)
    }

    @Test
    fun oldSettingsDefaultToDisabledProxies() {
        assertEquals(AppSettings(), Json.decodeFromString<AppSettings>("{}"))
    }

    @Test
    fun rejectsInvalidPortsAndEnabledPortConflicts() {
        assertFailsWith<IllegalArgumentException> { AppSettings(httpProxyPort = 0) }
        assertFailsWith<IllegalArgumentException> { AppSettings(socksProxyPort = 65536) }
        assertFailsWith<IllegalArgumentException> { AppSettings(httpProxyEnabled = true, socksProxyEnabled = true, socksProxyPort = 8080) }
        // Disabled listeners can keep their saved port even when another listener uses it.
        AppSettings(httpProxyEnabled = true, socksProxyPort = 8080)
    }

    @Test
    fun routeListPersistsAndCanBeCleared() = runTest {
        val routes = parseProxyRoutes(" 223.5.5.5/32\n\n10.0.0.0/8\n223.5.5.5/32 ")
        assertEquals(listOf("223.5.5.5/32", "10.0.0.0/8"), routes)
        repository.update { it.copy(proxyRoutes = routes) }
        assertEquals(routes, repository.settings.first().proxyRoutes)
        repository.update { it.copy(proxyRoutes = emptyList()) }
        assertEquals(emptyList(), repository.settings.first().proxyRoutes)
    }

    @Test
    fun routeListRejectsInvalidCidrs() {
        for (route in listOf("::/0", "1.2.3.4", "256.0.0.0/8", "1.2.3.4/33", "01.2.3.4/24", "name/8")) {
            assertFailsWith<IllegalArgumentException> { AppSettings(proxyRoutes = listOf(route)) }
        }
        AppSettings(proxyRoutes = listOf("0.0.0.0/0", "255.255.255.255/32", "10.0.0.1/24"))
    }

    @Test
    fun searchDomainsPersistInOrderAndCanBeCleared() = runTest {
        assertEquals(emptyList(), Json.decodeFromString<AppSettings>("{}").dnsSearchDomains)
        val domains = parseDnsSearchDomains(" Home.ARPA.\n\ncorp.example\nhome.arpa ")
        assertEquals(listOf("home.arpa", "corp.example"), domains)
        repository.update { it.copy(dnsSearchDomains = domains) }
        assertEquals(domains, repository.settings.first().dnsSearchDomains)
        repository.update { it.copy(dnsSearchDomains = emptyList()) }
        assertEquals(emptyList(), repository.settings.first().dnsSearchDomains)
    }

    @Test
    fun searchDomainsRejectMalformedSuffixes() {
        for (domain in listOf(".", "a..", "a..b", "-lan", "lan-", "*.example", "a_b", "1.2.3.4", "a b", "é.example", "a".repeat(64))) {
            assertFailsWith<IllegalArgumentException> { AppSettings(dnsSearchDomains = listOf(domain)) }
        }
        assertEquals(listOf(".", "a.."), parseDnsSearchDomains(".\na.."))
        AppSettings(dnsSearchDomains = listOf("lan", "my-domain.example."))
    }

    private companion object {
        const val OWN = "dev.fanchao.cpxy.vpn"
    }
}
