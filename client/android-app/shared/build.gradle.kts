import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.kotlin.multiplatform)
    alias(libs.plugins.android.kotlin.multiplatform.library)
    alias(libs.plugins.kotlinx.serialization)
    alias(libs.plugins.metro)
}

kotlin {
    android {
        namespace = "dev.fanchao.cpxy.shared"
        compileSdk = 36
        minSdk = 26

        compilerOptions {
            jvmTarget = JvmTarget.JVM_17
        }
    }

    jvm {
        compilerOptions {
            jvmTarget = JvmTarget.JVM_17
        }
    }

    sourceSets {
        commonMain.dependencies {
            implementation(libs.kotlinx.coroutines.core)
            implementation(libs.kotlinx.atomicfu)
            implementation(libs.kotlinx.serialization.json)
            implementation(libs.androidx.datastore.core)
            implementation(libs.androidx.datastore.preferences.core)
            implementation(libs.okio)
            implementation(libs.ktor.client.core)
            implementation(libs.ktor.client.websockets)
        }
        commonTest.dependencies {
            implementation(kotlin("test"))
            implementation(libs.kotlinx.coroutines.test)
        }
    }
}
