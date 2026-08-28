package dev.fanchao.cpxy

import android.app.Application
import android.content.Context
import dev.zacsweers.metro.createGraphFactory

class App : Application() {
    lateinit var graph: AndroidAppGraph
        private set

    override fun onCreate() {
        super.onCreate()
        graph = createGraphFactory<AndroidAppGraph.Factory>().create(this)

        // Resolve eager process observers once. Other graph bindings remain lazy.
        graph.clientServiceCoordinator
        graph.appController.eventsRepository.events
    }

    override fun onTerminate() {
        if (::graph.isInitialized) graph.appLifecycle.close()
        super.onTerminate()
    }
}

val Context.appGraph: AndroidAppGraph
    get() = (applicationContext as App).graph
