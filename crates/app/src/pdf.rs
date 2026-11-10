//! Re-exports of the shared PDF renderer.
//!
//! The renderer used to live in this module. It now lives in the
//! `pdf-render` crate so the gpui front-end uses the same implementation;
//! this shim keeps `crate::pdf::…` working unchanged for the Documents view.

pub use pdf_render::{Event, PageText, PdfWorker, Request};
