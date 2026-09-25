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
