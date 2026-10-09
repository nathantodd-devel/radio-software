// Packages the Rust library into an APK. The library is built separately:
//
//   cd mobile
//   cargo ndk -t arm64-v8a -t x86_64 -P 26 -o android/gradle/app/src/main/jniLibs build --release
//   cd android/gradle && ./gradlew assembleDebug
//
// or in one step with `./gradlew buildAll` from this directory.

buildscript {
    repositories {
        google()
        mavenCentral()
    }
    dependencies {
        classpath("com.android.tools.build:gradle:9.1.0")
        classpath("org.jetbrains.kotlin:kotlin-gradle-plugin:1.9.22")
    }
}

tasks.register("clean", Delete::class) {
    delete(rootProject.layout.buildDirectory)
}

// The Rust crate is two directories up, in mobile/.
val rustDir = rootProject.projectDir.parentFile.parentFile
val jniLibs = "android/gradle/app/src/main/jniLibs"
// The Android API level to link against: the app's minSdk. Below 26 there is
// no libnativewindow, which the renderer needs.
val apiLevel = "26"
// arm64-v8a is for phones; x86_64 is for the emulator, which can run arm64
// code but can't give it a working GPU.
val cargoNdk = listOf("cargo", "ndk", "-t", "arm64-v8a", "-t", "x86_64", "-P", apiLevel, "-o", jniLibs, "build")

tasks.register<Exec>("buildRustRelease") {
    group = "rust"
    description = "Compile the Rust library with cargo-ndk."
    workingDir = rustDir
    commandLine(cargoNdk + "--release")
}

tasks.register<Exec>("buildRustDebug") {
    group = "rust"
    description = "Compile the Rust library (debug) with cargo-ndk."
    workingDir = rustDir
    commandLine(cargoNdk)
}

tasks.register("buildAll") {
    group = "rust"
    description = "Build the Rust library (release), then the debug APK."
    dependsOn("buildRustRelease")
    finalizedBy(":app:assembleDebug")
}
