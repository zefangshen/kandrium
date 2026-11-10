//! Phase 0 spike for the egui -> gpui migration. **Disposable.**
//!
//! One binary, four probes selected by argv. Each answers one question from
//! `gpui-migration-plan.md` §11 and nothing more:
//!
//! ```text
//! cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- app
//! cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- board
//! cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- input
//! cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- pdf
//! cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- drag
//! ```
//!
//! `app` is the Stage 1 shell. The four probes predate it and stay for
//! reference.
//!
//! The PDF probe uses the shared `pdf-render` crate (the same worker the egui
//! app uses), not a copy.

mod board;
mod drag_probe;
mod input_probe;
mod pdf_probe;

#[cfg(test)]
mod harness_test;

fn main() {
    let _ = kandrium_core::logging::init("kandrium-gpui");
    kandrium_core::logging::install_panic_hook("kandrium-gpui");
    let mode = std::env::args().nth(1).unwrap_or_else(|| "app".into());
    match mode.as_str() {
        "app" => kandrium_app_gpui::run(),
        "board" => board::run(),
        "input" => input_probe::run(),
        "pdf" => pdf_probe::run(),
        "drag" => drag_probe::run(),
        other => {
            eprintln!(
                "usage: kandrium-gpui [app|board|input|pdf|drag] (got {other:?})\n\
                 env: KANDRIUM_DB overrides the database path, SPIKE_PDF the PDF"
            );
            std::process::exit(2);
        }
    }
}
