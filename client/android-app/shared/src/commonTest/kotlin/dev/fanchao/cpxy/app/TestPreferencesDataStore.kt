package dev.fanchao.cpxy.app

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.emptyPreferences
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

internal class TestPreferencesDataStore(
    initial: Preferences = emptyPreferences(),
) : DataStore<Preferences> {
    private val state = MutableStateFlow(initial)
    private val mutex = Mutex()
    var updateFailure: Throwable? = null
    val current: Preferences get() = state.value

    override val data: Flow<Preferences> = state

    override suspend fun updateData(transform: suspend (t: Preferences) -> Preferences): Preferences =
        mutex.withLock {
            updateFailure?.let { throw it }
            transform(state.value).also { state.value = it }
        }

    suspend fun replace(preferences: Preferences) {
        mutex.withLock { state.value = preferences }
    }
}
