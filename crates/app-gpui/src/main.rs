//! Kandrium desktop entry point.
//!
//! `app` (the default) starts the shell. `board`, `input`, `pdf` and `drag`
//! are the migration probes, kept for manual reference:
//!
//! ```text
//! cargo run --bin kandrium-app -- app
//! cargo run --bin kandrium-app -- board
//! cargo run --bin kandrium-app -- input
//! cargo run --bin kandrium-app -- pdf
//! cargo run --bin kandrium-app -- drag
//! ```
//!
//! The PDF probe uses the shared `pdf-render` crate, not a copy.

mod board;
mod drag_probe;
mod input_probe;
mod pdf_probe;

#[cfg(test)]
mod harness_test;

fn main() {
    let _ = kandrium_core::logging::init("kandrium-app");
    kandrium_core::logging::install_panic_hook("kandrium-app");
    let mode = std::env::args().nth(1).unwrap_or_else(|| "app".into());
    match mode.as_str() {
        "app" => kandrium_app::run(),
        "board" => board::run(),
        "input" => input_probe::run(),
        "pdf" => pdf_probe::run(),
        "drag" => drag_probe::run(),
        other => {
            eprintln!(
                "usage: kandrium-app [app|board|input|pdf|drag] (got {other:?})\n\
                 env: KANDRIUM_DB overrides the database path, SPIKE_PDF the PDF"
            );
            std::process::exit(2);
        }
    }
}
