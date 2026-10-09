//! Airspy Scanner for Android: the app's entry point and root view.
//!
//! The `android-activity` crate calls [`android_main`] on a thread of its
//! own once the activity has loaded this library. Build with
//! `cargo ndk -t arm64-v8a -t x86_64 -P 26 -o android/gradle/app/src/main/jniLibs build`,
//! then `./gradlew assembleDebug` in `android/gradle`.

// Link gpui-mobile so its JNI entry points are part of this library.
extern crate gpui_mobile;

#[cfg(target_os = "android")]
mod app {
    use gpui::{App, Application, Context, FontWeight, Window, WindowOptions, div, prelude::*, px, rgb};
    use gpui_mobile::android::jni;

    const LOG_TAG: &str = "airspy-scanner";
    /// Matches `gpui_surface` in the Android resources, so the splash screen
    /// hands over to the first frame without a flash.
    const BACKGROUND: u32 = 0x121318;
    const TEXT: u32 = 0xe6e9ee;
    const MUTED: u32 = 0x8b93a1;

    #[unsafe(no_mangle)]
    fn android_main(app: android_activity::AndroidApp) {
        android_logger::init_once(
            android_logger::Config::default()
                .with_max_level(log::LevelFilter::Info)
                .with_tag(LOG_TAG),
        );
        // Without this a panic kills the app with nothing in logcat.
        jni::install_panic_hook();

        jni::init_platform(&app);
        let Some(platform) = jni::shared_platform() else {
            log::error!("the Android platform didn't initialise");
            return;
        };
        // Blocks, driving the activity's event loop, until the activity is
        // destroyed. The closure runs once Android has given us a surface.
        Application::with_platform(platform.into_rc()).run(|cx: &mut App| {
            // Windows are always full screen here, so there are no bounds.
            let options = WindowOptions {
                window_bounds: None,
                ..Default::default()
            };
            if let Err(e) = cx.open_window(options, |_, cx| cx.new(|_| Scanner)) {
                log::error!("couldn't open the window: {e:#}");
            }
            cx.activate(true);
        });
    }

    /// The root view. A placeholder until the scanner itself is brought over.
    struct Scanner;

    impl Render for Scanner {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            // Keep clear of the status bar, notch and navigation bar.
            let insets = jni::platform()
                .and_then(|platform| platform.primary_window())
                .map(|window| window.safe_area_insets_logical());
            let (top, bottom, left, right) =
                insets.map_or((0.0, 0.0, 0.0, 0.0), |i| (i.top, i.bottom, i.left, i.right));
            div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .pt(px(top))
                .pb(px(bottom))
                .pl(px(left))
                .pr(px(right))
                .bg(rgb(BACKGROUND))
                .text_color(rgb(TEXT))
                .child(div().text_2xl().font_weight(FontWeight::BOLD).child("Airspy Scanner"))
                .child(div().text_color(rgb(MUTED)).child("Connect an Airspy to begin."))
        }
    }
}
