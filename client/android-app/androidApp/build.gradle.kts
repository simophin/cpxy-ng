import com.android.build.api.variant.ApplicationAndroidComponentsExtension
import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.file.ConfigurableFileCollection
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.FileSystemOperations
import org.gradle.api.provider.ListProperty
import org.gradle.api.provider.Property
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.InputFiles
import org.gradle.api.tasks.Internal
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.TaskAction
import org.gradle.process.ExecOperations
import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import java.io.ByteArrayOutputStream
import javax.inject.Inject

abstract class BuildAndroidRustLibraries : DefaultTask() {
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val rustInputs: ConfigurableFileCollection

    @get:Input
    abstract val rustTargets: ListProperty<String>

    @get:Input
    abstract val cargoNdkVersion: Property<String>

    @get:Input
    abstract val androidNdkVersion: Property<String>

    @get:Internal
    abstract val cargoWorkspaceDirectory: DirectoryProperty

    @get:Internal
    abstract val androidNdkDirectory: DirectoryProperty

    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty

    @get:Inject
    abstract val execOperations: ExecOperations

    @get:Inject
    abstract val fileSystemOperations: FileSystemOperations

    @TaskAction
    fun buildLibraries() {
        val expectedCargoNdkVersion = cargoNdkVersion.get()
        val cargoNdkVersionOutput = ByteArrayOutputStream()
        val cargoNdkVersionResult = execOperations.exec {
            commandLine("cargo", "ndk", "--version")
            standardOutput = cargoNdkVersionOutput
            errorOutput = cargoNdkVersionOutput
            isIgnoreExitValue = true
        }
        val installedCargoNdkVersion = cargoNdkVersionOutput.toString().trim()
        if (
            cargoNdkVersionResult.exitValue != 0 ||
                !Regex("""\b${Regex.escape(expectedCargoNdkVersion)}\b""")
                    .containsMatchIn(installedCargoNdkVersion)
        ) {
            throw GradleException(
                "Android Rust builds require cargo-ndk $expectedCargoNdkVersion. " +
                    "Install it with `cargo install cargo-ndk --version " +
                    "$expectedCargoNdkVersion --locked` (detected: " +
                    "${installedCargoNdkVersion.ifEmpty { "not installed" }})."
            )
        }

        val generatedDirectory = outputDirectory.get().asFile
        fileSystemOperations.delete { delete(generatedDirectory) }

        execOperations.exec {
            workingDir(cargoWorkspaceDirectory)
            environment("ANDROID_NDK_HOME", androidNdkDirectory.get().asFile.absolutePath)
            commandLine(
                listOf("cargo", "ndk") +
                    rustTargets.get().flatMap { listOf("-t", it) } +
                    listOf(
                        "-o",
                        generatedDirectory.absolutePath,
                        "build",
                        "--release",
                        "--locked",
                        "-p",
                        "client",
                        "--lib",
                    )
            )
        }
    }
}

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlinx.serialization)
    alias(libs.plugins.metro)
}

android {
    namespace = "dev.fanchao.cpxy"
    compileSdk = 37
    ndkVersion = libs.versions.androidNdk.get()

    defaultConfig {
        applicationId = "dev.fanchao.cpxy"
        minSdk = 26
        targetSdk = 36
        versionCode = 2
        versionName = providers.gradleProperty("cpxy.version").get()

        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    signingConfigs {
        getByName("debug") {
            storeFile = file("../debug.keystore")
            storePassword = "android"
            keyAlias = "androiddebugkey"
            keyPassword = "android"
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )

            signingConfig = signingConfigs.getByName("debug")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
    }
}

val cargoWorkspace = layout.projectDirectory.dir("../../..")
val generatedRustJniLibs = layout.buildDirectory.dir("generated/rustJniLibs")
val androidComponents = extensions.getByType<ApplicationAndroidComponentsExtension>()
val buildAndroidRustLibraries = tasks.register<BuildAndroidRustLibraries>("buildAndroidRustLibraries") {
    group = "build"
    description = "Builds the Rust client library for every supported Android ABI."
    cargoWorkspaceDirectory.set(cargoWorkspace)
    androidNdkDirectory.set(androidComponents.sdkComponents.ndkDirectory)
    outputDirectory.set(generatedRustJniLibs)
    rustTargets.set(
        listOf(
            "aarch64-linux-android",
            "armv7-linux-androideabi",
            "x86_64-linux-android",
            "i686-linux-android",
        )
    )
    cargoNdkVersion.set(libs.versions.cargoNdk)
    androidNdkVersion.set(libs.versions.androidNdk)
    rustInputs.from(
        cargoWorkspace.file("Cargo.lock"),
        cargoWorkspace.file("Cargo.toml"),
        cargoWorkspace.file("rust-toolchain.toml"),
        cargoWorkspace.dir(".cargo"),
        cargoWorkspace.asFileTree.matching {
            include("**/Cargo.toml")
            include("**/build.rs")
            include("**/*.rs")
            exclude("**/target/**")
            exclude("**/build/**")
        },
    )
}

androidComponents.onVariants(androidComponents.selector().all()) { variant ->
    variant.sources.jniLibs?.addGeneratedSourceDirectory(
        buildAndroidRustLibraries,
        BuildAndroidRustLibraries::outputDirectory,
    )
}


kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_17
    }
}

dependencies {

    implementation(project(":shared"))
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.ktx)
    implementation(libs.androidx.activity.compose)
    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.ui)
    implementation(libs.androidx.ui.graphics)
    implementation(libs.androidx.ui.tooling.preview)
    implementation(libs.androidx.material3)
    implementation(libs.kotlinx.coroutines.core)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.androidx.datastore.core)
    implementation(libs.androidx.datastore.preferences.core)
    implementation(libs.okio)
    implementation(libs.ktor.client.core)
    implementation(libs.ktor.client.cio)
    implementation(libs.ktor.client.websockets)
    debugImplementation(libs.androidx.ui.tooling)
    implementation(libs.jna) {
        artifact {
            type = "aar"
        }
    }
    testImplementation(libs.junit)
    testImplementation(libs.kotlinx.coroutines.test)
    testImplementation(libs.robolectric)
}
