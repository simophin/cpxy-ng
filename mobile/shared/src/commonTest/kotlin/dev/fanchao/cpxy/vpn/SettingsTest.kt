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

    private companion object {
        const val OWN = "dev.fanchao.cpxy.vpn"
    }
}
