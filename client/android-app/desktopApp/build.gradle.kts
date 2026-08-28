import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.gradle.api.DefaultTask
import org.gradle.api.GradleException
import org.gradle.api.file.ConfigurableFileCollection
import org.gradle.api.file.DirectoryProperty
import org.gradle.api.file.FileSystemOperations
import org.gradle.api.provider.Property
import org.gradle.api.tasks.Input
import org.gradle.api.tasks.InputDirectory
import org.gradle.api.tasks.InputFiles
import org.gradle.api.tasks.Internal
import org.gradle.api.tasks.OutputDirectory
import org.gradle.api.tasks.PathSensitive
import org.gradle.api.tasks.PathSensitivity
import org.gradle.api.tasks.Exec
import org.gradle.api.tasks.JavaExec
import org.gradle.api.tasks.SourceSetContainer
import org.gradle.api.tasks.TaskAction
import org.gradle.process.ExecOperations
import org.jetbrains.compose.desktop.application.dsl.TargetFormat
import java.nio.file.Files
import java.util.Locale
import javax.inject.Inject

data class DesktopRustPlatform(
    val id: String,
    val rustTarget: String,
    val libraryName: String,
)

fun detectDesktopRustPlatform(osName: String, architecture: String): DesktopRustPlatform {
    val os = osName.lowercase(Locale.ROOT)
    val arch = architecture.lowercase(Locale.ROOT)
    return when {
        os.contains("linux") && arch in setOf("amd64", "x86_64") ->
            DesktopRustPlatform("linux-x64", "x86_64-unknown-linux-gnu", "libclient.so")
        os.contains("windows") && arch in setOf("amd64", "x86_64") ->
            DesktopRustPlatform("windows-x64", "x86_64-pc-windows-msvc", "client.dll")
        (os.contains("mac") || os.contains("darwin")) && arch in setOf("amd64", "x86_64") ->
            DesktopRustPlatform("macos-x64", "x86_64-apple-darwin", "libclient.dylib")
        (os.contains("mac") || os.contains("darwin")) && arch in setOf("aarch64", "arm64") ->
            DesktopRustPlatform("macos-arm64", "aarch64-apple-darwin", "libclient.dylib")
        else -> throw GradleException(
            "Desktop Rust builds do not support OS '$osName' and architecture '$architecture'."
        )
    }
}

abstract class BuildDesktopRustLibrary : DefaultTask() {
    @get:InputFiles
    @get:PathSensitive(PathSensitivity.RELATIVE)
    abstract val rustInputs: ConfigurableFileCollection

    @get:Input abstract val rustTarget: Property<String>
    @get:Input abstract val libraryName: Property<String>
    @get:Internal abstract val cargoWorkspaceDirectory: DirectoryProperty
    @get:OutputDirectory abstract val outputDirectory: DirectoryProperty
    @get:Inject abstract val execOperations: ExecOperations
    @get:Inject abstract val fileSystemOperations: FileSystemOperations

    @TaskAction
    fun buildLibrary() {
        val workspace = cargoWorkspaceDirectory.get().asFile
        execOperations.exec {
            workingDir(workspace)
            commandLine(
                "cargo", "build", "--release", "--locked", "-p", "client", "--lib",
                "--target", rustTarget.get(),
            )
        }

        val cargoOutput = workspace.resolve(
            "target/${rustTarget.get()}/release/${libraryName.get()}"
        )
        if (!cargoOutput.isFile) {
            throw GradleException("Cargo completed but did not produce ${cargoOutput.absolutePath}")
        }

        val generatedDirectory = outputDirectory.get().asFile
        fileSystemOperations.delete { delete(generatedDirectory) }
        fileSystemOperations.copy {
            from(cargoOutput)
            into(generatedDirectory)
        }
    }
}

abstract class VerifyDesktopApplicationImage : DefaultTask() {
    @get:InputDirectory abstract val applicationImageDirectory: DirectoryProperty
    @get:Input abstract val libraryName: Property<String>

    @TaskAction
    fun verifyImage() {
        val imageDirectory = applicationImageDirectory.get().asFile.toPath()
        val libraries = Files.walk(imageDirectory).use { paths ->
            paths.filter { Files.isRegularFile(it) && it.fileName.toString() == libraryName.get() }
                .toList()
        }
        if (libraries.size != 1) {
            throw GradleException(
                "Expected exactly one '${libraryName.get()}' in Desktop application image " +
                    "'$imageDirectory', but found ${libraries.size}: $libraries"
            )
        }
        logger.lifecycle("Verified Desktop native resource: ${libraries.single()}")
    }
}

plugins {
    alias(libs.plugins.kotlin.jvm)
    alias(libs.plugins.jetbrains.compose)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.metro)
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
}

kotlin {
    compilerOptions {
        jvmTarget = JvmTarget.JVM_17
    }
}

dependencies {
    implementation(project(":shared"))
    implementation(compose.desktop.currentOs)
    implementation(libs.kotlinx.coroutines.core)
    implementation(libs.kotlinx.serialization.json)
    implementation(libs.androidx.datastore.core)
    implementation(libs.androidx.datastore.preferences.core)
    implementation(libs.okio)
    implementation(libs.ktor.client.core)
    implementation(libs.ktor.client.cio)
    implementation(libs.ktor.client.websockets)
    implementation(libs.jna)
    testImplementation(kotlin("test"))
}

val hostPlatform = detectDesktopRustPlatform(
    System.getProperty("os.name"),
    System.getProperty("os.arch"),
)
val cargoWorkspace = layout.projectDirectory.dir("../../..")
val generatedAppResources = layout.buildDirectory.dir("generated/appResources")
val generatedNativeDirectory = generatedAppResources.map { it.dir(hostPlatform.id) }
val desktopVersion = providers.gradleProperty("cpxy.version").get()
val applicationImagesRoot = layout.buildDirectory.dir("compose/binaries/main/app")
val buildDesktopRustLibrary = tasks.register<BuildDesktopRustLibrary>("buildDesktopRustLibrary") {
    group = "build"
    description = "Builds the Rust client library for this Desktop host."
    cargoWorkspaceDirectory.set(cargoWorkspace)
    rustTarget.set(hostPlatform.rustTarget)
    libraryName.set(hostPlatform.libraryName)
    outputDirectory.set(generatedNativeDirectory)
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

compose.desktop {
    application {
        mainClass = "dev.fanchao.cpxy.desktop.MainKt"
        nativeDistributions {
            appResourcesRootDir.set(generatedAppResources)
            targetFormats(TargetFormat.Dmg, TargetFormat.Msi, TargetFormat.Deb)
            packageName = "Cpxy"
            packageVersion = desktopVersion
            description = "Cross-platform proxy client"
            vendor = "Cpxy"
            linux {
                packageName = "cpxy"
            }
            macOS {
                bundleID = "dev.fanchao.cpxy"
            }
        }
    }
}

tasks.matching { it.name == "prepareAppResources" }.configureEach {
    dependsOn(buildDesktopRustLibrary)
}

tasks.withType<JavaExec>().configureEach {
    if (name == "run") {
        dependsOn(buildDesktopRustLibrary)
        doFirst {
            systemProperty(
                "cpxy.native.library.path",
                generatedNativeDirectory.get().file(hostPlatform.libraryName).asFile.absolutePath,
            )
        }
    }
}

val mainSourceSet = extensions.getByType<SourceSetContainer>().named("main")
tasks.register<JavaExec>("desktopNativeSmoke") {
    group = "verification"
    description = "Loads the generated Desktop native library and checks its ABI and error path."
    dependsOn(buildDesktopRustLibrary, tasks.named("classes"))
    classpath = mainSourceSet.get().runtimeClasspath
    mainClass.set("dev.fanchao.cpxy.desktop.NativeSmokeProbe")
    doFirst {
        systemProperty(
            "cpxy.native.library.path",
            generatedNativeDirectory.get().file(hostPlatform.libraryName).asFile.absolutePath,
        )
    }
}
val verifyDesktopApplicationImage = tasks.register<VerifyDesktopApplicationImage>(
    "verifyDesktopApplicationImage"
) {
    group = "verification"
    description = "Checks that the Desktop application image contains exactly one host library."
    dependsOn(tasks.named("createDistributable"))
    applicationImageDirectory.set(applicationImagesRoot)
    libraryName.set(hostPlatform.libraryName)
}

tasks.register<Exec>("packagedNativeProbe") {
    group = "verification"
    description = "Runs the packaged Desktop launcher in headless native-probe mode."
    dependsOn(verifyDesktopApplicationImage)
    doFirst {
        val imageRoot = applicationImagesRoot.get().asFile
        val launcher = when {
            hostPlatform.id == "linux-x64" -> imageRoot.resolve("Cpxy/bin/Cpxy")
            hostPlatform.id == "windows-x64" -> imageRoot.resolve("Cpxy/Cpxy.exe")
            hostPlatform.id.startsWith("macos-") ->
                imageRoot.resolve("Cpxy.app/Contents/MacOS/Cpxy")
            else -> throw GradleException("No packaged launcher path for ${hostPlatform.id}")
        }
        if (!launcher.isFile) {
            throw GradleException("Packaged Desktop launcher is missing: ${launcher.absolutePath}")
        }
        executable = launcher.absolutePath
        args("--native-probe")
    }
}
