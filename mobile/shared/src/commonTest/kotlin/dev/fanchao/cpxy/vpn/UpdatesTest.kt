package dev.fanchao.cpxy.vpn

import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.cancel
import kotlinx.coroutines.test.runTest
import okio.FileSystem
import kotlin.random.Random
import kotlin.test.AfterTest
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertNotNull
import kotlin.test.assertNull

class AppVersionTest {
    @Test
    fun parsesTagsAndDescribedCommits() {
        assertEquals(AppVersion(1, 17, 0, 0), AppVersion.parse("1.17.0"))
        assertEquals(AppVersion(1, 17, 0, 5), AppVersion.parse("1.17.0-5-gabc1234"))
        assertEquals(AppVersion(2, 0, 1, 0), AppVersion.parse("v2.0.1"))
    }

    @Test
    fun rejectsOtherNames() {
        for (name in listOf("dev", "abc1234", "1.17", "1.17.0-5", "1.17.0-rc1", "1.17.0-5-gabc1234-dirty")) {
            assertNull(AppVersion.parse(name), name)
        }
    }

    @Test
    fun ordersCommitsAfterTheirTagAndNumerically() {
        val sorted = listOf("1.9.0", "1.17.0-10-gaaaaaaa", "1.17.0", "1.17.0-9-gbbbbbbb", "1.18.0", "1.10.0")
            .sortedBy { AppVersion.parse(it)!! }
        assertEquals(listOf("1.9.0", "1.10.0", "1.17.0", "1.17.0-9-gbbbbbbb", "1.17.0-10-gaaaaaaa", "1.18.0"), sorted)
    }
}

class UpdateCheckerTest {
    private val directory = FileSystem.SYSTEM_TEMPORARY_DIRECTORY / "cpxy-vpn-test-${Random.nextLong().toULong()}"
    private val scope = CoroutineScope(Dispatchers.Default + Job())
    private val settings = SettingsRepository.create(directory / "settings.preferences_pb", scope)
    private var clock = 10 * UpdateChecker.CHECK_INTERVAL_MILLIS

    private class FakeUpdater(var latest: String?) : Updater {
        var checks = 0
        val installed = mutableListOf<String>()
        override suspend fun latestVersion(): String? = latest.also { checks++ }
        override suspend fun install(versionName: String, onProgress: (Float?) -> Unit) {
            onProgress(0.5f)
            installed += versionName
        }
    }

    @AfterTest
    fun cleanUp() {
        scope.cancel()
        FileSystem.SYSTEM.deleteRecursively(directory)
    }

    private fun checker(updater: Updater, current: String = "1.17.0-5-gabc1234") =
        UpdateChecker(updater, current, settings, scope, { clock })

    @Test
    fun offersANewerVersion() = runTest {
        val checker = checker(FakeUpdater("1.17.0-6-gdef5678"))
        checker.checkIfDue()!!.join()
        assertEquals(UpdateState.Available("1.17.0-6-gdef5678"), checker.state.value)
        assertEquals("1.17.0-6-gdef5678", checker.prompt.value)
    }

    @Test
    fun ignoresTheSameOrAnOlderVersion() = runTest {
        for (latest in listOf("1.17.0-5-gabc1234", "1.17.0", null)) {
            val checker = checker(FakeUpdater(latest))
            checker.checkNow()!!.join()
            assertEquals(UpdateState.UpToDate, checker.state.value, "latest $latest")
            assertNull(checker.prompt.value)
        }
    }

    @Test
    fun checksByItselfOnlyOnceADay() = runTest {
        val updater = FakeUpdater("1.17.0-6-gdef5678")
        checker(updater).checkIfDue()!!.join()
        clock += UpdateChecker.CHECK_INTERVAL_MILLIS - 1
        checker(updater).checkIfDue()!!.join()
        assertEquals(1, updater.checks)

        clock += 1
        checker(updater).checkIfDue()!!.join()
        assertEquals(2, updater.checks)
    }

    @Test
    fun doesNotOfferASkippedVersionAgain() = runTest {
        val updater = FakeUpdater("1.17.0-6-gdef5678")
        val first = checker(updater)
        first.checkIfDue()!!.join()
        first.skip("1.17.0-6-gdef5678").join()
        assertNull(first.prompt.value)

        clock += UpdateChecker.CHECK_INTERVAL_MILLIS
        val again = checker(updater)
        again.checkIfDue()!!.join()
        assertNull(again.prompt.value)
        // Still installable from the settings
        assertEquals(UpdateState.Available("1.17.0-6-gdef5678"), again.state.value)

        updater.latest = "1.17.0-7-g9999999"
        clock += UpdateChecker.CHECK_INTERVAL_MILLIS
        val newer = checker(updater)
        newer.checkIfDue()!!.join()
        assertEquals("1.17.0-7-g9999999", newer.prompt.value)
    }

    @Test
    fun installs() = runTest {
        val updater = FakeUpdater("1.17.0-6-gdef5678")
        val checker = checker(updater)
        checker.install("1.17.0-6-gdef5678")!!.join()
        assertEquals(UpdateState.Installing, checker.state.value)
        assertEquals(listOf("1.17.0-6-gdef5678"), updater.installed)
    }

    @Test
    fun doesOneThingAtATime() = runTest {
        val checker = checker(FakeUpdater("1.17.0-6-gdef5678"))
        val check = assertNotNull(checker.checkNow())
        assertNull(checker.checkNow())
        check.join()
    }

    @Test
    fun reportsFailures() = runTest {
        val checker = checker(object : Updater {
            override suspend fun latestVersion(): String? = error("offline")
            override suspend fun install(versionName: String, onProgress: (Float?) -> Unit) = error("bad checksum")
        })
        checker.checkNow()!!.join()
        assertEquals(UpdateState.Failed("offline"), checker.state.value)
        checker.install("1.17.0-6-gdef5678")!!.join()
        assertEquals(UpdateState.Failed("bad checksum"), checker.state.value)
    }

    @Test
    fun isOffForLocalBuilds() = runTest {
        val updater = FakeUpdater("1.17.0-6-gdef5678")
        val checker = checker(updater, current = "dev")
        assertFalse(checker.enabled)
        assertNull(checker.checkIfDue())
        assertNull(checker.checkNow())
        assertEquals(0, updater.checks)
    }
}
