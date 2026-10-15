package dev.fanchao.cpxy.vpn

import android.view.KeyEvent
import androidx.compose.material3.MaterialTheme
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.input.key.Key
import androidx.compose.ui.semantics.SemanticsActions
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.v2.createComposeRule
import androidx.test.platform.app.InstrumentationRegistry
import dev.fanchao.cpxy.vpn.ui.AppPickerScreen
import dev.fanchao.cpxy.vpn.ui.CpxyVpnApp
import dev.fanchao.cpxy.vpn.ui.EditProfileScreen
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import okio.Path.Companion.toOkioPath
import org.junit.After
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import java.io.File

class NavigationTest {
    @get:Rule val compose = createComposeRule()
    private val scope = CoroutineScope(Dispatchers.IO + SupervisorJob())
    private val directory = File(InstrumentationRegistry.getInstrumentation().targetContext.cacheDir, "navigation-${System.nanoTime()}")
    private val repository = ProfileRepository.create(File(directory, "profiles.preferences_pb").toOkioPath(), scope)
    private val settings = SettingsRepository.create(File(directory, "settings.preferences_pb").toOkioPath(), scope)
    private val original = Profile("original", "Home", "https://:key@example.com", listOf("223.5.5.5"), listOf("tcp://1.1.1.1?via=cpxy"))
    private var alwaysOnOpened = false
    private val platform = object : Platform {
        override val openAlwaysOnSettings: () -> Unit = { alwaysOnOpened = true }
        override val addQuickSettingsTile: (() -> Unit)? = null
        override val versionName = "test"
        override val apps = object : AppCatalog {
            override suspend fun apps() = emptyList<InstalledApp>()
            override suspend fun icon(packageName: String): ImageBitmap? = null
        }
    }
    private val controller = object : VpnController {
        override val state = MutableStateFlow<VpnState>(VpnState.Disconnected)
        override val traffic = MutableStateFlow(Traffic())
        override suspend fun connectionsSince(since: Long, limit: Int) = ConnectionPage(emptyList(), 0)
        override suspend fun connectionsBefore(before: Long?, limit: Int) = ConnectionPage(emptyList(), 0)
        override suspend fun connect(profile: Profile) {}
        override fun disconnect() {}
    }

    @After fun cleanUp() {
        scope.cancel()
        directory.deleteRecursively()
    }

    private fun showApp() {
        compose.setContent { CpxyVpnApp(repository, settings, controller, platform) }
    }

    @Test fun dpadEntersSettingsAndActivatesRows() {
        showApp()
        val tab = compose.onNode(hasText("Settings") and hasClickAction())
        tab.performClick()
        tab.performSemanticsAction(SemanticsActions.RequestFocus) { it() }
        tab.performKeyInput { pressKey(Key.DirectionUp) }
        tab.assertIsNotFocused()
        repeat(20) {
            if (compose.onAllNodes(hasText("Always-on VPN") and isFocused()).fetchSemanticsNodes().isEmpty()) {
                compose.onRoot().performKeyInput { pressKey(Key.DirectionUp) }
            }
        }
        compose.onNodeWithText("Always-on VPN").assertIsFocused()
        compose.onNodeWithText("Always-on VPN").performKeyInput { pressKey(Key.DirectionCenter) }
        compose.runOnIdle { assertTrue(alwaysOnOpened) }
        compose.onNodeWithText("Always-on VPN").performKeyInput { pressKey(Key.DirectionDown) }
        compose.onNodeWithText("All apps").assertIsFocused()
        compose.onNodeWithText("All apps").performKeyInput { pressKey(Key.DirectionDown); pressKey(Key.DirectionCenter) }
        compose.waitUntil { runBlocking { settings.settings.first().appFilter == AppFilter.OnlySelected } }
    }

    private fun press(key: Key) {
        compose.onRoot().performKeyInput { pressKey(key) }
    }

    private fun back() {
        InstrumentationRegistry.getInstrumentation().sendKeyDownUpSync(KeyEvent.KEYCODE_BACK)
        compose.waitForIdle()
    }

    private fun focusTab(label: String) {
        compose.onNode(hasText(label) and hasClickAction())
            .performSemanticsAction(SemanticsActions.RequestFocus) { it() }
    }

    private fun moveUpTo(label: String) {
        repeat(20) {
            if (compose.onAllNodes(hasText(label) and isFocused()).fetchSemanticsNodes().isEmpty()) {
                press(Key.DirectionUp)
            }
        }
        compose.onNodeWithText(label).assertIsFocused()
    }

    @Test fun dpadEntersVpnFromEveryTabAndTheAddButton() {
        runBlocking { repository.save(original) }
        showApp()
        compose.waitUntil { compose.onAllNodesWithText("Home").fetchSemanticsNodes().isNotEmpty() }
        for (label in listOf("VPN", "Traffic", "Settings")) {
            focusTab(label)
            press(Key.DirectionUp)
            compose.onNode(hasText(label) and hasClickAction()).assertIsNotFocused()
            compose.onNode(isFocused() and (hasText("Home") or hasText("Connect to Home") or hasContentDescription("Actions for Home"))).assertExists()
        }
        val add = compose.onNodeWithContentDescription("Add profile")
        add.performSemanticsAction(SemanticsActions.RequestFocus) { it() }
        press(Key.DirectionUp)
        add.assertIsNotFocused()
        compose.onNode(isFocused() and (hasText("Home") or hasText("Connect to Home") or hasContentDescription("Actions for Home"))).assertExists()
    }

    @Test fun dpadOpensProfileMenuAndReturnsToSelectionAndTabs() {
        runBlocking { repository.save(original) }
        showApp()
        compose.waitUntil { compose.onAllNodesWithText("Home").fetchSemanticsNodes().isNotEmpty() }
        focusTab("VPN")
        press(Key.DirectionUp)
        press(Key.DirectionLeft)
        moveUpTo("Home")
        press(Key.DirectionRight)
        val menu = compose.onNodeWithContentDescription("Actions for Home")
        menu.assertIsFocused()
        press(Key.DirectionLeft)
        compose.onNodeWithText("Home").assertIsFocused()
        press(Key.DirectionRight)
        press(Key.DirectionCenter)
        compose.onNodeWithText("Edit").assertIsDisplayed()
        compose.onNodeWithText("Duplicate").assertIsDisplayed()
        back()
        compose.waitUntil { compose.onAllNodesWithText("Edit").fetchSemanticsNodes().isEmpty() }
        menu.assertIsFocused()
        press(Key.DirectionLeft)
        compose.onNodeWithText("Home").assertIsFocused()
        repeat(5) { press(Key.DirectionDown) }
        compose.onNode(isFocused() and (hasText("VPN") or hasText("Traffic") or hasText("Settings"))).assertExists()
    }

    @Test fun dpadActivatesBothAppPickerButtons() {
        showApp()
        compose.onNode(hasText("Settings") and hasClickAction()).performClick()
        for (label in listOf("Only selected apps", "All apps except selected")) {
            focusTab("Settings")
            press(Key.DirectionUp)
            press(Key.DirectionLeft)
            moveUpTo(label)
            press(Key.DirectionRight)
            compose.onNode(isFocused() and hasText("Choose")).assertExists()
            press(Key.DirectionLeft)
            compose.onNodeWithText(label).assertIsFocused()
            press(Key.DirectionRight)
            press(Key.DirectionCenter)
            compose.onNodeWithText(if (label == "Only selected apps") "Apps using the VPN" else "Apps bypassing the VPN").assertIsDisplayed()
            compose.onNodeWithContentDescription("Back").assertIsDisplayed()
            back()
            compose.waitUntil { compose.onAllNodesWithText(label).fetchSemanticsNodes().isNotEmpty() }
            compose.onNodeWithText(label).assertExists()
        }
    }

    @Test fun dpadMovesFromAppPickerTopBarThroughSearchToApps() {
        val catalog = object : AppCatalog {
            override suspend fun apps() = listOf(InstalledApp("com.example.alpha", "Alpha", isSystem = false))
            override suspend fun icon(packageName: String): ImageBitmap? = null
        }
        compose.setContent { MaterialTheme { AppPickerScreen(AppFilter.OnlySelected, settings, catalog, {}) } }
        val search = compose.onNode(hasSetTextAction())
        val alpha = compose.onNode(hasText("Alpha") and isToggleable())
        compose.waitUntil { compose.onAllNodes(hasText("Alpha") and isToggleable()).fetchSemanticsNodes().isNotEmpty() }
        for (action in listOf("Back", "More options")) {
            compose.onNodeWithContentDescription(action).performSemanticsAction(SemanticsActions.RequestFocus) { it() }
            press(Key.DirectionDown)
            search.assertIsFocused()
        }
        press(Key.DirectionDown)
        alpha.assertIsFocused()
        press(Key.DirectionCenter)
        compose.waitUntil { runBlocking { "com.example.alpha" in settings.settings.first().appsFor(AppFilter.OnlySelected) } }
        press(Key.DirectionUp)
        search.assertIsFocused()
        press(Key.DirectionUp)
        compose.onNodeWithContentDescription("Back").assertIsFocused()
    }

    @Test fun duplicateIsPrefilledAndSavedSeparately() {
        runBlocking { repository.save(original) }
        showApp()
        compose.waitUntil { compose.onAllNodesWithContentDescription("Actions for Home").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithContentDescription("Actions for Home").performClick()
        compose.onNodeWithText("Duplicate").performClick()
        compose.waitUntil { compose.onAllNodesWithText("Home (copy)").fetchSemanticsNodes().isNotEmpty() }
        compose.onNodeWithText("Home (copy)").assertExists()
        compose.onNodeWithText(original.server).assertExists()
        compose.onNodeWithContentDescription("Delete profile").assertDoesNotExist()
        assertEquals(listOf(original), runBlocking { repository.profiles.first().profiles })
        compose.onNodeWithText("Save").performScrollTo().performClick()
        compose.waitUntil { runBlocking { repository.profiles.first().profiles.size == 2 } }
        val stored = runBlocking { repository.profiles.first() }
        assertEquals(original, stored.profiles.first())
        val copy = stored.profiles.last()
        assertNotEquals(original.id, copy.id)
        assertEquals(original.copy(id = copy.id, name = "Home (copy)"), copy)
        assertEquals(copy.id, stored.selectedId)
    }

    @Test fun typingLongDnsListKeepsTheFieldStableAboveKeyboard() {
        compose.setContent { MaterialTheme { EditProfileScreen(null, repository, {}) } }
        val field = compose.onNode(hasText("Alternative DNS") and hasSetTextAction())
        field.performScrollTo().performClick()
        field.performTextReplacement((1..12).joinToString("\n") { "tcp://1.1.1.1?via=cpxy" })
        compose.waitForIdle()
        val before = field.fetchSemanticsNode().boundsInRoot
        repeat(8) {
            field.performTextInput("x")
            compose.waitForIdle()
            val after = field.fetchSemanticsNode().boundsInRoot
            assertEquals("Typing must not move the form vertically", before.top, after.top, 2f)
            assertEquals("DNS field height must stay bounded", before.height, after.height, 2f)
        }
    }

    @Test fun nextMovesThroughFieldsAndDoneValidatesAndSaves() {
        var done = false
        compose.setContent { MaterialTheme { EditProfileScreen(null, repository, { done = true }) } }
        val name = compose.onNode(hasText("Name") and hasSetTextAction())
        val server = compose.onNode(hasText("Server") and hasSetTextAction())
        val upstream = compose.onNode(hasText("Upstream DNS") and hasSetTextAction())
        val alternative = compose.onNode(hasText("Alternative DNS") and hasSetTextAction())
        name.performClick().performTextInput("New")
        name.performImeAction()
        server.assertIsFocused()
        server.performImeAction()
        upstream.assertIsFocused().performImeAction()
        alternative.assertIsFocused().performImeAction()
        compose.onNodeWithText("Server must not be blank").assertExists()
        compose.runOnIdle { assertFalse(done) }
        server.performScrollTo().performClick().performTextInput(original.server)
        server.performImeAction()
        upstream.performImeAction()
        alternative.performImeAction()
        compose.waitUntil { done }
        assertEquals("New", runBlocking { repository.profiles.first().selected?.name })
    }
}
