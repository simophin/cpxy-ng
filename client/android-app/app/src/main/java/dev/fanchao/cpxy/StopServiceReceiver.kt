package dev.fanchao.cpxy

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import dev.fanchao.cpxy.App.Companion.appInstance

class StopServiceReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        context.appInstance.clientInstanceManager.stop()
    }
}