// `java` inside a Gradle Kotlin script resolves to the Java plugin extension,
// not to the package, so these types have to be imported by name.
import java.io.File
import java.util.Properties

plugins {
    id("com.android.application")
}

android {
    namespace = "com.fenghanli.dspprobe"
    compileSdk = 36

    defaultConfig {
        applicationId = "com.fenghanli.dspprobe"
        minSdk = 29
        targetSdk = 36
        versionCode = 1
        versionName = "0.1-probe"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

// --- Rust DSP core ---------------------------------------------------------
// cargo-ndk drops libdsp.so into src/main/jniLibs, where normal Android
// packaging picks it up. Deliberately a plain `cargo` invocation rather than
// externalNativeBuild: the crate is self-contained and has no CMake in it.
//
// Only arm64-v8a is built — that is the only ABI this project targets, and
// building the other three would triple the Rust compile time for nothing.

val sdkDirFromProps: String = rootProject.file("local.properties")
    .takeIf { it.exists() }
    ?.let { f ->
        Properties().apply { f.inputStream().use { load(it) } }.getProperty("sdk.dir")
    }
    .orEmpty()

val sdkDirForNdk: String = sdkDirFromProps.ifEmpty { System.getenv("ANDROID_HOME").orEmpty() }

val ndkHome: String = file("$sdkDirForNdk/ndk")
    .listFiles()
    ?.map { it.absolutePath }
    ?.maxOrNull()
    .orEmpty()

// The Gradle daemon inherits the PATH from whenever it was started, which almost
// never includes ~/.cargo/bin. Resolve cargo by path rather than hoping, and put
// its directory on PATH because `cargo ndk` finds the cargo-ndk subcommand by
// searching PATH for `cargo-ndk`.
val cargoBin: File? = listOf(
    File(System.getProperty("user.home"), ".cargo/bin/cargo"),
    File("/opt/homebrew/bin/cargo"),
    File("/usr/local/bin/cargo"),
).firstOrNull { it.canExecute() }

val cargoPathForBuild: String = buildString {
    cargoBin?.parentFile?.let { append(it.absolutePath).append(':') }
    append(System.getenv("PATH").orEmpty())
    append(":/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin")
}

val cargoNdkBuild = tasks.register<Exec>("cargoNdkBuild") {
    group = "build"
    description = "Cross-compiles the Rust DSP core into src/main/jniLibs"
    workingDir = rootProject.file("rust")
    commandLine(
        cargoBin?.absolutePath ?: "cargo", "ndk",
        "-t", "arm64-v8a",
        "-o", "${projectDir}/src/main/jniLibs",
        "build", "--release",
    )
    environment("ANDROID_HOME", sdkDirForNdk)
    environment("ANDROID_NDK_HOME", ndkHome)
    environment("PATH", cargoPathForBuild)
    // Fail loudly rather than silently shipping an APK with no DSP in it.
    doFirst {
        check(cargoBin != null) {
            "cargo not found. Install Rust from https://rustup.rs and re-run."
        }
        check(ndkHome.isNotEmpty()) {
            "No Android NDK found under '$sdkDirForNdk/ndk'. Install one with: sdkmanager \"ndk;29.0.14206865\""
        }
    }
}

tasks.named("preBuild") {
    dependsOn(cargoNdkBuild)
}
