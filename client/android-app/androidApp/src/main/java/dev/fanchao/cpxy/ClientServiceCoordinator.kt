package dev.fanchao.cpxy

import dev.fanchao.cpxy.app.ProfileInstanceManager
import android.content.Context
import android.content.Intent
import android.widget.Toast
import kotlinx.coroutines.Dispatchers
import dev.fanchao.cpxy.app.AppScope
import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

@Inject
@SingleIn(AppScope::class)
class ClientServiceCoordinator (
    appContext: Context,
    profileInstanceManager: ProfileInstanceManager,
    applicationScope: CoroutineScope,
) {
    init {
        applicationScope.launch {
            profileInstanceManager.state
                .map { state -> state.startedResult?.isSuccess == true }
                .distinctUntilChanged()
                .collect { shouldStartService ->
                    val intent = Intent(appContext, ClientService::class.java)
                    if (shouldStartService) {
                        appContext.startService(intent)
                    } else {
                        appContext.stopService(intent)
                    }
                }
        }

        applicationScope.launch(Dispatchers.Main) {
            profileInstanceManager.state
                .map { state ->  state.startedResult?.exceptionOrNull() }
                .filterNotNull()
                .distinctUntilChanged()
                .collect {
                    Toast.makeText(appContext, "${it.message}", Toast.LENGTH_LONG).show()
                }
        }
    }

}
