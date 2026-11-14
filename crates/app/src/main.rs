//! Kandrium desktop entry point.
//!
//! Starts the `kandrium-app` shell. Any argument is rejected so a mistyped
//! launcher fails loudly instead of silently opening the app.

fn main() {
    let _ = kandrium_core::logging::init("kandrium-app");
    kandrium_core::logging::install_panic_hook("kandrium-app");
    if let Some(arg) = std::env::args().nth(1) {
        eprintln!("usage: kandrium-app (got {arg:?})");
        std::process::exit(2);
    }
    kandrium_app::run();
}
