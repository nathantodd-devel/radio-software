//! Airspy Scanner for Android.
//!
//! The `android-activity` crate calls [`android_main`] on a thread of its
//! own once the activity has loaded this library. Build with
//! `cargo ndk -t arm64-v8a -t x86_64 -P 26 -o android/gradle/app/src/main/jniLibs build`,
//! then `./gradlew assembleDebug` in `android/gradle`.

// Link gpui-mobile so its JNI entry points are part of this library.
extern crate gpui_mobile;

#[cfg(target_os = "android")]
mod ui;
#[cfg(target_os = "android")]
mod usb;

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
fn android_main(app: android_activity::AndroidApp) {
    use gpui::{App, AppContext, Application, WindowOptions};
    use gpui_mobile::android::jni;

    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("airspy-scanner"),
    );
    // Without this a panic kills the app with nothing in logcat.
    jni::install_panic_hook();

    // The app's private storage: the channel database and recordings go here.
    let data_dir = app.internal_data_path().unwrap_or_default();

    jni::init_platform(&app);
    let Some(platform) = jni::shared_platform() else {
        log::error!("the Android platform didn't initialise");
        return;
    };
    // Blocks, driving the activity's event loop, until the activity is
    // destroyed. The closure runs once Android has given us a surface.
    Application::with_platform(platform.into_rc()).run(move |cx: &mut App| {
        // Windows are always full screen here, so there are no bounds.
        let options = WindowOptions {
            window_bounds: None,
            ..Default::default()
        };
        let opened = cx.open_window(options, |_, cx| cx.new(|cx| ui::ScannerApp::new(data_dir.clone(), cx)));
        if let Err(e) = opened {
            log::error!("couldn't open the window: {e:#}");
        }
        cx.activate(true);
    });
    usb::close();
}
