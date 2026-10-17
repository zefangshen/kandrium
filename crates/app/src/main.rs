//! Kandrium desktop app entry point (eframe/egui).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod app;
mod dnd;
mod icons;
mod theme;
mod views;
mod watcher;
mod widgets;

use eframe::egui;

/// Create (once) the dock icon NSImage, returned as a raw pointer value.
/// Returns 0 when the icon file could not be loaded.
#[cfg(target_os = "macos")]
fn dock_icon_image() -> usize {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};

    static CACHE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CACHE.get_or_init(|| unsafe {
        let icon_path = concat!(env!("CARGO_MANIFEST_DIR"), "/packaging/icon.png");
        let c_path = std::ffi::CString::new(icon_path).unwrap();
        let ns_string_cls = Class::get("NSString").unwrap();
        let ns_image_cls = Class::get("NSImage").unwrap();
        let path: *mut Object = msg_send![ns_string_cls, stringWithUTF8String:c_path.as_ptr()];
        let image: *mut Object = msg_send![ns_image_cls, alloc];
        let image: *mut Object = msg_send![image, initWithContentsOfFile:path];
        image as usize
    })
}

/// Apply the dock icon when running as a bare binary (dev via `cargo run`).
/// Bundled `.app` builds get their icon from Contents/Resources/*.icns.
/// Returns true when the icon is confirmed applied (or not needed).
///
/// Must be called from the main thread once the app is running; calls made
/// while NSApplication is still launching are silently clobbered.
#[cfg(target_os = "macos")]
pub fn set_dock_icon() -> bool {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};
    use std::sync::atomic::{AtomicBool, Ordering};

    // Bundled app: macOS already uses the bundle's .icns.
    let in_bundle = std::env::current_exe()
        .map(|p| p.to_string_lossy().contains(".app/Contents/MacOS/"))
        .unwrap_or(false);
    if in_bundle {
        return true;
    }

    let image = dock_icon_image() as *mut Object;
    if image.is_null() {
        return false;
    }

    // One-line diagnostic on the first attempt only.
    static LOGGED: AtomicBool = AtomicBool::new(false);

    unsafe {
        let ns_app_cls = Class::get("NSApplication").unwrap();
        let app: *mut Object = msg_send![ns_app_cls, sharedApplication];
        let before: *mut Object = msg_send![app, applicationIconImage];
        let _: () = msg_send![app, setApplicationIconImage:image];
        let after: *mut Object = msg_send![app, applicationIconImage];
        let setter_ok = after == image || after != before;

        // Fallback: override the dock tile's content view directly. On some
        // macOS versions setApplicationIconImage: is a no-op for bare,
        // unbundled executables; the tile override still works there.
        let tile: *mut Object = msg_send![app, dockTile];
        let mut tile_ok = false;
        if !tile.is_null() {
            let iv_cls = Class::get("NSImageView").unwrap();
            let iv: *mut Object = msg_send![iv_cls, alloc];
            let iv: *mut Object = msg_send![iv, init];
            let _: () = msg_send![iv, setImage:image];
            let _: () = msg_send![tile, setContentView:iv];
            let _: () = msg_send![tile, display];
            tile_ok = true;
        }

        let ok = setter_ok || tile_ok;
        if !LOGGED.swap(true, Ordering::Relaxed) {
            eprintln!(
                "[kandrium] dock icon: setter_ok={setter_ok} tile_ok={tile_ok} \
                 (before={before:p} after={after:p} image={image:p})"
            );
        }
        ok
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set_dock_icon() -> bool {
    true
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([940.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Kandrium",
        options,
        Box::new(|cc| Ok(Box::new(app::KandriumApp::new(cc)))),
    )
}
