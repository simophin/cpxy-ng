package dev.fanchao.cpxy

import android.app.Application
import android.content.Context
import com.sun.jna.Native
import kotlinx.coroutines.flow.MutableStateFlow

class App : Application() {
    val currentConfiguration: MutableStateFlow<ClientConfiguration?> = MutableStateFlow(null)

    val client: Client by lazy {
        Native.load("client", Client::class.java) as Client
    }

    companion object {
        val Context.appInstance: App
            get() = (applicationContext as App)
    }
}