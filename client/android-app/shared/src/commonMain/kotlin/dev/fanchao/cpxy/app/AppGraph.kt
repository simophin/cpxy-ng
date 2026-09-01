package dev.fanchao.cpxy.app

import dev.zacsweers.metro.Inject
import dev.zacsweers.metro.SingleIn

abstract class AppScope private constructor()

interface AppGraph {
    val appController: AppController
    val appLifecycle: AppLifecycle
}

@Inject
@SingleIn(AppScope::class)
class AppController(
    val configRepository: ConfigRepository,
    val profileInstanceManager: ProfileInstanceManager,
    val eventsRepository: EventsRepository,
)
