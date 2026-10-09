//! The scanner's browser client: a GPUI app compiled to WebAssembly. The
//! scanning is done by scanner-web, which serves this and is what it talks
//! to.

mod audio;
mod net;
mod ui;

use gpui::prelude::*;
use gpui::{App, Bounds, WindowBounds, WindowOptions, px, size};

fn main() {
    gpui_platform::web_init();
    let app = gpui_platform::application();
    keep_alive(&app);
    app.run(|cx: &mut App| {
        // The window is the page: these bounds are only a starting point.
        let bounds = Bounds::centered(None, size(px(1100.), px(760.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        cx.open_window(options, |_, cx| cx.new(ui::Client::new))
            .expect("failed to open window");
        cx.activate(true);
    });
}

/// Stop the application from being dropped once it has started.
///
/// In a browser `Application::run` returns straight away, and at this
/// revision of GPUI nothing else owns the application, so it and its window
/// would be dropped as soon as start-up finished. `Application` is a wrapper
/// around a single `Rc`; this leaks one more reference to it.
fn keep_alive(app: &gpui::Application) {
    use std::rc::Rc;
    assert_eq!(size_of::<gpui::Application>(), size_of::<Rc<()>>());
    // SAFETY: an `Rc` is a pointer to its counts followed by its value,
    // whatever the value's type, and the copy is forgotten, not dropped.
    let app: Rc<()> = unsafe { std::mem::transmute_copy(app) };
    std::mem::forget(Rc::clone(&app));
    std::mem::forget(app);
}
