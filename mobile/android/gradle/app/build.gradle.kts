// Packages the pre-compiled Rust library into an APK whose activity hosts
// the GPUI app. The library must be built first and placed in
// app/src/main/jniLibs/<abi>/; see ../build.gradle.kts.

plugins {
    id("com.android.application")
}

android {
    namespace = "dev.airspyscanner"
    compileSdk = 34

    defaultConfig {
        applicationId = "dev.airspyscanner"
        minSdk = 26          // Vulkan 1.0 is mandatory from API 26+
        targetSdk = 34
        // A release build passes these in (-PversionCode=…, -PversionName=…).
        versionCode = (findProperty("versionCode") as String?)?.toInt() ?: 1
        versionName = (findProperty("versionName") as String?) ?: "0.2.0"

        // Tell NativeActivity which .so to load.
        // This must match the library name in mobile/Cargo.toml.
        ndk {
            // Phones, and the emulator.
            abiFilters += listOf("arm64-v8a", "x86_64")
        }

        // Forward the library name to the manifest via a placeholder.
        manifestPlaceholders["nativeLibraryName"] = "airspy_scanner_mobile"
    }

    // Release builds are signed with the keystore these environment variables
    // describe. Without them the release APK comes out unsigned, which
    // Android won't install.
    val keystore = System.getenv("ANDROID_KEYSTORE_FILE")
    signingConfigs {
        if (keystore != null) {
            create("release") {
                storeFile = file(keystore)
                storePassword = System.getenv("ANDROID_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("ANDROID_KEY_ALIAS")
                keyPassword = System.getenv("ANDROID_KEY_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            signingConfig = signingConfigs.findByName("release")
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
        debug {
            isDebuggable = true
            isJniDebuggable = true
        }
    }

    // We do NOT use CMake / ndk-build — the native library is compiled
    // externally via cargo-ndk and placed directly into jniLibs.
    //
    // Disable the built-in native build system so Gradle doesn't look for
    // a CMakeLists.txt or Android.mk.
    externalNativeBuild {
        // Intentionally left empty.
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_1_8
        targetCompatibility = JavaVersion.VERSION_1_8
    }

    // Tell Gradle where the pre-built .so files live.
    sourceSets {
        getByName("main") {
            jniLibs.srcDirs("src/main/jniLibs")
        }
    }

    packaging {
        // cargo-ndk also copies gpui-mobile's own shared library, which is
        // already linked into ours; leave the duplicate out of the APK.
        jniLibs.excludes += "**/libgpui_mobile.so"

        // Prevent stripping of the Rust library — cargo already strips in
        // release mode and stripping again can break backtraces.
        jniLibs {
            keepDebugSymbols += listOf(
                "*/arm64-v8a/libairspy_scanner_mobile.so",
                "*/armeabi-v7a/libairspy_scanner_mobile.so",
                "*/x86_64/libairspy_scanner_mobile.so",
                "*/x86/libairspy_scanner_mobile.so"
            )
        }
    }

    // Lint is relaxed: the Java here is gpui-mobile's, not ours to fix.
    lint {
        abortOnError = false
        checkReleaseBuilds = false
    }
}

dependencies {
    // AndroidX core for NotificationCompat (used by GpuiNotifications)
    implementation("androidx.core:core:1.12.0")
    // AndroidX SplashScreen compat (used by GpuiActivity to hold splash until native init)
    implementation("androidx.core:core-splashscreen:1.0.1")
    // AndroidX Biometric for BiometricPrompt (used by GpuiAuthActivity)
    implementation("androidx.biometric:biometric:1.1.0")
    // AndroidX Media for MediaSessionCompat (used by GpuiMediaSession for system controls)
    implementation("androidx.media:media:1.7.1")
}
