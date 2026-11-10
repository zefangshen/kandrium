//! Phase 0 spike for the egui -> gpui migration. **Disposable.**
//!
//! One binary, four probes selected by argv. Each answers one question from
//! `gpui-migration-plan.md` §11 and nothing more:
//!
//! ```text
//! cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- board
//! cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- input
//! cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- pdf
//! cargo run --manifest-path crates/app-gpui/Cargo.toml --bin kandrium-gpui -- drag
//! ```
//!
//! The PDF module is the *existing* `crates/app/src/pdf.rs`, pulled in by
//! path so the spike reuses the real renderer without touching `crates/app`.

// The PDF module is the *existing* app module; its own dead-code allowances
// live in `crates/app`, so silence them here rather than editing that file.
#[allow(dead_code)]
#[path = "../../app/src/pdf.rs"]
mod pdf;

mod board;
mod drag_probe;
mod input_probe;
mod pdf_probe;

#[cfg(test)]
mod harness_test;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "board".into());
    match mode.as_str() {
        "board" => board::run(),
        "input" => input_probe::run(),
        "pdf" => pdf_probe::run(),
        "drag" => drag_probe::run(),
        other => {
            eprintln!(
                "usage: kandrium-gpui [board|input|pdf|drag] (got {other:?})\n\
                 env: KANDRIUM_DB overrides the database path, SPIKE_PDF the PDF"
            );
            std::process::exit(2);
        }
    }
}
