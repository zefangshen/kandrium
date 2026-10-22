//! Kandrium desktop app entry point (eframe/egui).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod anim;
mod app;
mod dnd;
mod icons;
mod pdf;
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

/// True when the running executable lives in a `.app` that actually ships an
/// `.icns` icon. Packaged builds do; the minimal dev bundle
/// (`target/Kandrium-dev.app`) has only `Contents/MacOS` and an `Info.plist`,
/// so it still needs the runtime icon.
#[cfg(target_os = "macos")]
fn bundle_ships_icon() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    for ancestor in exe.ancestors() {
        if ancestor.file_name().and_then(|name| name.to_str()) != Some("Contents") {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(ancestor.join("Resources")) else {
            return false;
        };
        return entries.flatten().any(|entry| {
            entry.path().is_file()
                && entry
                    .path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("icns"))
        });
    }
    false
}

/// Apply the dock icon when the executable has no bundled `.icns` (bare
/// `cargo run` binaries and the dev `.app`). Packaged `.app` builds get their
/// icon from `Contents/Resources/*.icns` and skip this.
/// Returns true when the icon is confirmed applied (or not needed).
///
/// Must be called from the main thread once the app is running; calls made
/// while NSApplication is still launching are silently clobbered.
#[cfg(target_os = "macos")]
pub fn set_dock_icon() -> bool {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};
    use std::sync::atomic::{AtomicBool, Ordering};

    // Packaged app with a real .icns: macOS already uses the bundle icon.
    if bundle_ships_icon() {
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

/// Measured macOS traffic-light geometry, in window (top-left origin)
/// coordinates: `(band_center_y, title_left_x)`.
///
/// `band_center_y` is the vertical center of the traffic-light buttons —
/// the line the title-bar band content is centered on. `title_left_x` is
/// where title-bar content should start (zoom-button right edge plus a gap).
/// Measured fresh on every call: the title-bar layout settles a few frames
/// after window creation, so an early cached value would stick at the
/// transient position. Returns `None` while the window is not yet up.
#[cfg(target_os = "macos")]
pub fn traffic_light_metrics() -> Option<(f32, f32)> {
    ensure_titlebar_overlay();
    measure_traffic_lights()
}

#[cfg(target_os = "macos")]
fn measure_traffic_lights() -> Option<(f32, f32)> {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};

    /// NSWindowButton: Close=0, Miniaturize=1, Zoom=2. All three are
    /// measured so an enum renumbering can't silently hit the wrong button.
    const BUTTONS: [i64; 3] = [0, 1, 2];
    /// Gap between the traffic lights' right edge and the title text.
    const TITLE_GAP: f64 = 14.0;
    /// Cap on change-detection logs (settling can take several frames).
    const LOG_CAP: usize = 20;
    static LOGGED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static LAST: std::sync::Mutex<Option<(i32, i32)>> = std::sync::Mutex::new(None);

    unsafe {
        let ns_app = Class::get("NSApplication")?;
        let app: *mut Object = msg_send![ns_app, sharedApplication];
        let win: *mut Object = msg_send![app, mainWindow];
        if win.is_null() {
            return None;
        }
        let win_frame: NSRect = msg_send![win, frame];
        if win_frame.size.height <= 0.0 {
            return None;
        }

        let mut center_y: Option<f64> = None;
        let mut right_edge: f64 = 0.0;
        for (i, kind) in BUTTONS.iter().enumerate() {
            let btn: *mut Object = msg_send![win, standardWindowButton:*kind];
            if btn.is_null() {
                return None;
            }
            // convertRect: expects a rect in the RECEIVER's coordinate
            // space, so it must be `bounds` (origin 0,0). Passing `frame`
            // double-counts the superview offset and misplaces both axes.
            let bounds: NSRect = msg_send![btn, bounds];
            let in_window: NSRect =
                msg_send![btn, convertRect:bounds toView:std::ptr::null_mut::<Object>()];
            if i == 0 {
                // Window base coordinates have a bottom-left origin; egui's
                // y axis points down from the window top.
                center_y = Some(
                    win_frame.size.height - (in_window.origin.y + in_window.size.height / 2.0),
                );
            }
            right_edge = right_edge.max(in_window.origin.x + in_window.size.width);
        }
        let m = (center_y? as f32, (right_edge + TITLE_GAP) as f32);

        // Log only when the measured value changes (title-bar settling).
        let key = ((m.0 * 2.0).round() as i32, (m.1 * 2.0).round() as i32);
        let mut last = LAST.lock().ok()?;
        if *last != Some(key) && LOGGED.load(std::sync::atomic::Ordering::Relaxed) < LOG_CAP {
            eprintln!(
                "[kandrium] traffic lights: center_y={} title_x={} frame_h={}",
                m.0, m.1, win_frame.size.height
            );
            LOGGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *last = Some(key);
        }
        Some(m)
    }
}

/// Minimal Foundation geometry types for the ObjC calls below.
#[cfg(target_os = "macos")]
mod ns_geometry {
    use objc::{Encode, Encoding};

    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct NSPoint {
        pub x: f64,
        pub y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct NSSize {
        pub width: f64,
        pub height: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug)]
    pub struct NSRect {
        pub origin: NSPoint,
        pub size: NSSize,
    }
    unsafe impl Encode for NSPoint {
        fn encode() -> Encoding {
            unsafe { Encoding::from_str("{_NSPoint=dd}") }
        }
    }
    unsafe impl Encode for NSSize {
        fn encode() -> Encoding {
            unsafe { Encoding::from_str("{_NSSize=dd}") }
        }
    }
    unsafe impl Encode for NSRect {
        fn encode() -> Encoding {
            unsafe { Encoding::from_str("{_NSRect={_NSPoint=dd}{_NSSize=dd}}") }
        }
    }
}

#[cfg(target_os = "macos")]
use ns_geometry::NSRect;

/// Enforce the window styles the title-bar band depends on: the egui surface
/// must extend under the OS title-bar strip (`fullSizeContentView`), with a
/// transparent strip and hidden title so only the traffic lights overlay it.
/// The viewport builder asks winit for exactly this, but it silently does
/// not stick on every launch — applied here directly and idempotently.
#[cfg(target_os = "macos")]
fn ensure_titlebar_overlay() {
    use objc::runtime::{Class, Object};
    use objc::{msg_send, sel, sel_impl};

    /// NSWindowStyleMask.fullSizeContentView
    const FULL_SIZE_CONTENT_VIEW: u64 = 1 << 15;
    /// NSWindowTitleVisibility.titleVisibilityHidden
    const TITLE_HIDDEN: i64 = 1;

    unsafe {
        let Some(ns_app) = Class::get("NSApplication") else {
            return;
        };
        let app: *mut Object = msg_send![ns_app, sharedApplication];
        let win: *mut Object = msg_send![app, mainWindow];
        if win.is_null() {
            return;
        }

        let mask: u64 = msg_send![win, styleMask];
        let content_view: *mut Object = msg_send![win, contentView];
        let content_frame: NSRect = msg_send![content_view, frame];
        let transparent: i8 = msg_send![win, titlebarAppearsTransparent];
        let title_visibility: i64 = msg_send![win, titleVisibility];

        let mut missing: Vec<&str> = Vec::new();
        if mask & FULL_SIZE_CONTENT_VIEW == 0 {
            missing.push("fullSizeContentView");
            let _: () = msg_send![win, setStyleMask:(mask | FULL_SIZE_CONTENT_VIEW)];
        }
        if transparent == 0 {
            missing.push("titlebarAppearsTransparent");
            let _: () = msg_send![win, setTitlebarAppearsTransparent:1i8];
        }
        if title_visibility != TITLE_HIDDEN {
            missing.push("titleVisibility");
            let _: () = msg_send![win, setTitleVisibility:TITLE_HIDDEN];
        }
        if !missing.is_empty() {
            eprintln!(
                "[kandrium] title-bar overlay: applied {:?} (mask was {mask:#x}, \
                 content frame {content_frame:?})",
                missing
            );
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn traffic_light_metrics() -> Option<(f32, f32)> {
    None
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([940.0, 600.0])
            // macOS: draw the app's own title bar in the OS title-bar strip.
            // The content view fills the whole window, the native title-bar
            // background and title text are hidden, and the traffic-light
            // buttons stay visible over the toolbar's title-bar row.
            .with_fullsize_content_view(true)
            .with_titlebar_shown(false)
            .with_title_shown(false),
        ..Default::default()
    };

    eframe::run_native(
        "Kandrium",
        options,
        Box::new(|cc| Ok(Box::new(app::KandriumApp::new(cc)))),
    )
}
