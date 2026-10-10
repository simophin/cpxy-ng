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

/** Builds `libmobile_engine.so` for every ABI with cargo-ndk. */
abstract class BuildEngineLibraries : DefaultTask() {
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
                        "mobile-engine",
                        "--lib",
                    )
            )
        }

        // cargo-ndk also copies the cdylibs of dependencies, such as libclient.so.
        generatedDirectory.walk()
            .filter { it.isFile && it.name != "libmobile_engine.so" }
            .forEach { it.delete() }
    }
}

/**
 * Generates the UniFFI Kotlin bindings. They are read from a host debug build of the engine,
 * because the release libraries are stripped of the UniFFI metadata.
 */
abstract class GenerateEngineBindings : DefaultTask() {
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val rustInputs: ConfigurableFileCollection

    @get:Internal
    abstract val cargoWorkspaceDirectory: DirectoryProperty

    @get:OutputDirectory
    abstract val outputDirectory: DirectoryProperty

    @get:Inject
    abstract val execOperations: ExecOperations

    @get:Inject
    abstract val fileSystemOperations: FileSystemOperations

    @TaskAction
    fun generate() {
        val generatedDirectory = outputDirectory.get().asFile
        fileSystemOperations.delete { delete(generatedDirectory) }

        val workspace = cargoWorkspaceDirectory.get().asFile
        val cargo = listOf("cargo")
        val features = listOf("--locked", "-p", "mobile-engine", "--features", "bindgen")
        execOperations.exec {
            workingDir(workspace)
            commandLine(cargo + listOf("build", "--lib") + features)
        }

        val targetDirectory = System.getenv("CARGO_TARGET_DIR")?.let(::File)
            ?: workspace.resolve("target")
        val libraryName = if (System.getProperty("os.name").startsWith("Mac")) {
            "libmobile_engine.dylib"
        } else {
            "libmobile_engine.so"
        }
        execOperations.exec {
            workingDir(workspace)
            commandLine(
                cargo + listOf("run", "--bin", "uniffi-bindgen") + features + listOf(
                    "--",
                    "generate",
                    "--library",
                    targetDirectory.resolve("debug/$libraryName").absolutePath,
                    "--language",
                    "kotlin",
                    "--no-format",
                    "--out-dir",
                    generatedDirectory.absolutePath,
                )
            )
        }
    }
}

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
}

android {
    namespace = "dev.fanchao.cpxy.vpn"
    compileSdk = 37
    ndkVersion = libs.versions.androidNdk.get()

    defaultConfig {
        applicationId = "dev.fanchao.cpxy.vpn"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = providers.gradleProperty("cpxy.vpn.version").get()
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

    packaging {
        jniLibs {
            // JNA ships these; the engine is not built for them.
            excludes += listOf("lib/armeabi/**", "lib/mips/**", "lib/mips64/**")
        }
    }
}

val cargoWorkspace = layout.projectDirectory.dir("../..")
val rustInputFiles = files(
    cargoWorkspace.file("Cargo.lock"),
    cargoWorkspace.file("Cargo.toml"),
    cargoWorkspace.file("rust-toolchain.toml"),
    cargoWorkspace.dir(".cargo"),
    cargoWorkspace.asFileTree.matching {
        include("**/Cargo.toml")
        include("**/build.rs")
        include("**/*.rs")
        include("mobile-engine/uniffi.toml")
        exclude("**/target/**")
        exclude("**/build/**")
    },
)
val androidComponents = extensions.getByType<ApplicationAndroidComponentsExtension>()

val buildEngineLibraries = tasks.register<BuildEngineLibraries>("buildEngineLibraries") {
    group = "build"
    description = "Builds the Rust engine library for every supported Android ABI."
    cargoWorkspaceDirectory.set(cargoWorkspace)
    androidNdkDirectory.set(androidComponents.sdkComponents.ndkDirectory)
    outputDirectory.set(layout.buildDirectory.dir("generated/engineJniLibs"))
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
    rustInputs.from(rustInputFiles)
}

val generateEngineBindings = tasks.register<GenerateEngineBindings>("generateEngineBindings") {
    group = "build"
    description = "Generates the Kotlin bindings of the Rust engine."
    cargoWorkspaceDirectory.set(cargoWorkspace)
    outputDirectory.set(layout.buildDirectory.dir("generated/engineBindings"))
    rustInputs.from(rustInputFiles)
}

androidComponents.onVariants(androidComponents.selector().all()) { variant ->
    variant.sources.jniLibs?.addGeneratedSourceDirectory(
        buildEngineLibraries,
        BuildEngineLibraries::outputDirectory,
    )
    variant.sources.kotlin?.addGeneratedSourceDirectory(
        generateEngineBindings,
        GenerateEngineBindings::outputDirectory,
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
    implementation(libs.androidx.activity.compose)
    implementation(libs.compose.runtime)
    implementation(libs.compose.ui)
    implementation(libs.kotlinx.coroutines.core)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.jna) {
        artifact {
            type = "aar"
        }
    }
}
