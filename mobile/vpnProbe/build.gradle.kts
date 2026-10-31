// Test fixture only; never bundled in the VPN app.
plugins { alias(libs.plugins.android.application) }
android {
    namespace = "dev.fanchao.cpxy.probe"
    compileSdk = 37
    defaultConfig {
        applicationId = "dev.fanchao.cpxy.probe"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "test"
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
