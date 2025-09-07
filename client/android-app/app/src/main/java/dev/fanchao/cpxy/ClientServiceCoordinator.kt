package dev.fanchao.cpxy

import android.content.Context
import android.content.Intent
import kotlinx.coroutines.DelicateCoroutinesApi
import kotlinx.coroutines.GlobalScope
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.launch

@OptIn(DelicateCoroutinesApi::class)
class ClientServiceCoordinator (
    appContext: Context,
    clientInstanceManager: ClientInstanceManager,
    repository: ClientConfigurationRepository
) {
    init {
        GlobalScope.launch {
            combine(
                clientInstanceManager.started,
                repository.configurations
            ) { started, configs -> started && configs.count { it.enabled } > 0 }
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
    }

}