//! Kandrium on gpui — Stage 1 foundation plus Stage 2 board interactivity.
//!
//! Contract of the migration: this crate only composes views. Every query,
//! every derivation and every mutation lives in `kandrium-core`. Reads use a
//! read-only handle; writes go through a handle bootstrapped exactly like the
//! egui app and the MCP server.

pub mod anim;
pub mod app;
pub mod assets;
pub mod settings;
pub mod store;
pub mod theme;
pub mod views;
pub mod vim;
pub mod watcher;

use gpui_kit::{AppContext, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, px, size};

/// Open the app window and run the event loop.
pub fn run() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        theme::install_component_theme(cx);
        theme::install_fonts(cx);
        cx.set_window_appearance(Some(gpui_kit::WindowAppearance::Dark));

        let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
        gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Kandrium".into()),
                    // The section row is the titlebar: the window content starts
                    // at the top edge and the native traffic lights float over
                    // the left inset. `title` still names the window for the
                    // OS (Mission Control, accessibility); nothing draws it.
                    appears_transparent: true,
                    // The light container is `button_height + 2 * y`; the
                    // standard button frame is 16 px, so 14 makes the native
                    // titlebar container exactly the 44 px section row.
                    traffic_light_position: Some(point(px(12.), px(14.))),
                }),
                // The app draws its own titlebar row and decides where the
                // window may be dragged. Without this, AppKit owns titlebar
                // dragging and moves the window from every press in the row —
                // including the section chips.
                app_owns_titlebar_drag: true,
                ..Default::default()
            },
            cx,
            |_window, cx| cx.new(|_| app::AppShell::new()),
        )
        .expect("open the Kandrium window");

        cx.activate(true);
    });
}
