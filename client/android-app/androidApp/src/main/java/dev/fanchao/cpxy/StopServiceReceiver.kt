package dev.fanchao.cpxy

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

class StopServiceReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent?) {
        context.appGraph.appController.configRepository.setProfileEnabled(null)
    }
}
