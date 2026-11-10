//! Kandrium on gpui — Stage 1 foundation.
//!
//! Tenant of the migration: this crate only composes views. Every query and
//! every derivation lives in `kandrium-core`, and the database handle is
//! read-only so the egui app and this front-end never contend for the write
//! lock.

pub mod app;
pub mod assets;
pub mod store;
pub mod theme;
pub mod views;

use gpui_kit::{AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};

/// Open the app window and run the event loop.
pub fn run() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        theme::install_fonts(cx);

        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Kandrium".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            cx,
            |_window, cx| cx.new(|_| app::AppShell::new()),
        )
        .expect("open the Kandrium window");

        cx.activate(true);
    });
}
