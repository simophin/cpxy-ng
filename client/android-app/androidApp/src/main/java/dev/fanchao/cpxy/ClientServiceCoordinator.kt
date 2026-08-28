package dev.fanchao.cpxy

import android.content.Context
import android.content.Intent
import android.widget.Toast
import kotlinx.coroutines.DelicateCoroutinesApi
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.GlobalScope
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.filterNotNull
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch

@OptIn(DelicateCoroutinesApi::class)
class ClientServiceCoordinator (
    appContext: Context,
    profileInstanceManager: ProfileInstanceManager,
) {
    init {
        GlobalScope.launch {
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

        GlobalScope.launch(Dispatchers.Main) {
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