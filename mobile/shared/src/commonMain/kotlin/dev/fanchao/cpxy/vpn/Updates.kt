package dev.fanchao.cpxy.vpn

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.launch

/**
 * The version names CI gives builds: a tag such as `1.17.0`, or `git describe` of a later commit
 * on main, such as `1.17.0-5-gabc1234`. The download server orders its version folders the same
 * way (download-worker/src/index.js).
 */
data class AppVersion(val major: Int, val minor: Int, val patch: Int, val commits: Int) : Comparable<AppVersion> {
    // Not semver: `1.17.0-5` comes after `1.17.0`, not before it.
    override fun compareTo(other: AppVersion): Int =
        compareValuesBy(this, other, { it.major }, { it.minor }, { it.patch }, { it.commits })

    companion object {
        private val pattern = Regex("""v?(\d{1,9})\.(\d{1,9})\.(\d{1,9})(?:-(\d{1,9})-g[0-9a-f]+)?""")

        /** Null for names CI does not make, such as `dev` for local builds. */
        fun parse(name: String): AppVersion? = pattern.matchEntire(name)?.destructured?.let { (major, minor, patch, commits) ->
            AppVersion(major.toInt(), minor.toInt(), patch.toInt(), commits.ifEmpty { "0" }.toInt())
        }
    }
}

/** Finds and installs published versions of the app. */
interface Updater {
    /** The version name the server's `latest` points at, or null when nothing is published. */
    suspend fun latestVersion(): String?

    /**
     * Downloads [versionName], checks it against the published checksums and hands it to the
     * system installer, which asks the user to confirm. [onProgress] gets the fraction
     * downloaded, or null while the size is unknown.
     */
    suspend fun install(versionName: String, onProgress: (Float?) -> Unit)
}

sealed interface UpdateState {
    data object Idle : UpdateState
    data object Checking : UpdateState
    data object UpToDate : UpdateState
    data class Available(val versionName: String) : UpdateState
    data class Downloading(val progress: Float?) : UpdateState

    /** Handed to the system installer, which now asks the user. */
    data object Installing : UpdateState
    data class Failed(val message: String) : UpdateState
}

/**
 * Checks for a newer version: by itself at most once a day, offering what it finds unless the user
 * skipped that version, or when asked from the settings.
 */
class UpdateChecker(
    private val updater: Updater,
    currentVersion: String,
    private val settingsRepository: SettingsRepository,
    private val scope: CoroutineScope,
    private val now: () -> Long,
) {
    private val current = AppVersion.parse(currentVersion)
    private val mutableState = MutableStateFlow<UpdateState>(UpdateState.Idle)
    private val mutablePrompt = MutableStateFlow<String?>(null)
    private var job: Job? = null

    /** False for builds CI did not make, which have no version to compare. */
    val enabled: Boolean get() = current != null

    val state: StateFlow<UpdateState> = mutableState.asStateFlow()

    /** A version found by the daily check, to offer the user until they answer. */
    val prompt: StateFlow<String?> = mutablePrompt.asStateFlow()

    // Each action returns its job, or null when it does nothing (for the tests).

    fun checkIfDue(): Job? = launchUnlessBusy {
        val settings = settingsRepository.settings.first()
        if (now() - settings.lastUpdateCheck < CHECK_INTERVAL_MILLIS) return@launchUnlessBusy
        val found = check() ?: return@launchUnlessBusy
        if (found != settings.skippedUpdate) mutablePrompt.value = found
    }

    fun checkNow(): Job? = launchUnlessBusy { check() }

    fun skip(versionName: String): Job {
        mutablePrompt.value = null
        return scope.launch { settingsRepository.update { it.copy(skippedUpdate = versionName) } }
    }

    fun dismissPrompt() {
        mutablePrompt.value = null
    }

    fun install(versionName: String): Job? {
        mutablePrompt.value = null
        return launchUnlessBusy {
            mutableState.value = UpdateState.Downloading(null)
            mutableState.value = try {
                updater.install(versionName) { mutableState.value = UpdateState.Downloading(it) }
                UpdateState.Installing
            } catch (e: CancellationException) {
                throw e
            } catch (e: Exception) {
                UpdateState.Failed(e.message ?: "The update failed")
            }
        }
    }

    private fun launchUnlessBusy(block: suspend CoroutineScope.() -> Unit): Job? {
        if (!enabled || job?.isActive == true) return null
        return scope.launch(block = block).also { job = it }
    }

    /** The system installer gave up, or the user cancelled it. */
    fun installFailed(message: String) {
        mutableState.value = UpdateState.Failed(message)
    }

    /** The newer version found, if any. */
    private suspend fun check(): String? {
        mutableState.value = UpdateState.Checking
        val latest = try {
            updater.latestVersion()
        } catch (e: CancellationException) {
            throw e
        } catch (e: Exception) {
            mutableState.value = UpdateState.Failed(e.message ?: "Could not check for updates")
            return null
        }
        settingsRepository.update { it.copy(lastUpdateCheck = now()) }
        val newer = latest?.takeIf { name -> AppVersion.parse(name)?.let { current != null && it > current } == true }
        mutableState.value = newer?.let { UpdateState.Available(it) } ?: UpdateState.UpToDate
        return newer
    }

    companion object {
        const val CHECK_INTERVAL_MILLIS = 24 * 60 * 60 * 1000L
    }
}
