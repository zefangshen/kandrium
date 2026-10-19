//! PDF rendering backend for the Documents section.
//!
//! One worker thread per open document owns the parsed
//! [`hayro_syntax::Pdf`] and its [`RenderCache`] (both carry non-`Send`
//! interior state, so they must stay on one thread). The UI sends requests
//! over a channel and drains [`Event`]s each frame — the same
//! background-work pattern as [`crate::watcher`].
//!
//! Page geometry uses *page view space*: points, origin at the top-left of
//! the displayed page (rotation applied), y down — the space hayro renders
//! in at scale 1. Screen mapping is a pure `× zoom + offset`.

use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};

use hayro::hayro_interpret::hayro_cmap::BfString;
use hayro::hayro_interpret::hayro_syntax::Pdf;
use hayro::hayro_interpret::hayro_syntax::page::Page;
use hayro::hayro_interpret::{
    BlendMode, ClipPath, Context, Device, GlyphDrawMode, Image, InterpreterCache,
    InterpreterSettings, Paint, PathDrawMode, SoftMask, TransformExt, interpret_page,
};
use kurbo::{Affine, Point, Rect, Shape};

// ---------------------------------------------------------------------------
// Text model
// ---------------------------------------------------------------------------

/// One glyph on a page, in page view space (y down).
#[derive(Debug, Clone)]
pub struct TextGlyph {
    pub text: String,
    /// Bounding box: `[x0, y0, x1, y1]`.
    pub rect: [f32; 4],
    /// Em-box corners: `[x, y]` × 4.
    pub quad: [[f32; 2]; 4],
}

/// A run of glyphs grouped onto one baseline.
#[derive(Debug, Clone)]
pub struct TextLine {
    /// Union rect of the line's glyphs: `[x0, y0, x1, y1]`.
    pub rect: [f32; 4],
    /// Indices into [`PageText::glyphs`], in reading order.
    pub glyphs: Vec<usize>,
}

/// Extracted text for one page.
#[derive(Debug, Clone, Default)]
pub struct PageText {
    pub glyphs: Vec<TextGlyph>,
    pub lines: Vec<TextLine>,
}

impl PageText {
    /// Glyphs of one line, in order.
    pub fn line_glyphs(&self, line: &TextLine) -> impl Iterator<Item = &TextGlyph> {
        line.glyphs.iter().filter_map(|i| self.glyphs.get(*i))
    }

    /// Highlight quads for every glyph whose rect intersects `sel` — both
    /// in page view space. One quad per line (union of the hit glyphs'
    /// rects); lines whose glyphs are rotated (em box ≠ rect) fall back to
    /// per-glyph quads so rotated text highlights correctly. This is what
    /// a drag selection turns into when the highlight tool is active.
    pub fn selection_quads(&self, sel: &[f32; 4]) -> Vec<[[f32; 2]; 4]> {
        let hits = |g: &TextGlyph| crate::pdf::rects_intersect(sel, &g.rect);
        self.lines
            .iter()
            // Fast skip: lines whose union rect misses the selection.
            .filter(|line| crate::pdf::rects_intersect(sel, &line.rect))
            .flat_map(|line| self.line_selection_quads(line, &hits))
            .collect()
    }

    fn line_selection_quads(
        &self,
        line: &TextLine,
        hits: &dyn Fn(&TextGlyph) -> bool,
    ) -> Vec<[[f32; 2]; 4]> {
        let selected: Vec<&TextGlyph> = self.line_glyphs(line).filter(|g| hits(g)).collect();
        if selected.is_empty() {
            return Vec::new();
        }
        // Rotated glyphs: their stored em-box quad deviates from the
        // axis-aligned rect, so merged rect quads would over-cover — emit
        // each glyph's own quad instead.
        let rotated = selected
            .iter()
            .any(|g| !approx_quad_eq(&g.quad, &quad_from_rect(g.rect)));
        if rotated {
            return selected.iter().map(|g| g.quad).collect();
        }
        let rect = selected.iter().fold(selected[0].rect, |r, g| {
            [
                r[0].min(g.rect[0]),
                r[1].min(g.rect[1]),
                r[2].max(g.rect[2]),
                r[3].max(g.rect[3]),
            ]
        });
        vec![quad_from_rect(rect)]
    }

    /// Plain-text content of the glyphs selected by `sel` (line order),
    /// used to prefill a highlight's memo.
    pub fn selection_text(&self, sel: &[f32; 4]) -> String {
        let hits = |g: &TextGlyph| crate::pdf::rects_intersect(sel, &g.rect);
        let mut out = String::new();
        for line in &self.lines {
            if !crate::pdf::rects_intersect(sel, &line.rect) {
                continue;
            }
            for g in self.line_glyphs(line) {
                if hits(g) {
                    out.push_str(&g.text);
                }
            }
            out.push('\n');
        }
        out.trim_end().to_string()
    }
}

/// Order-insensitive quad ≈ rect check: a glyph is "rotated" when its
/// stored em-box quad cannot be reproduced as the axis-aligned rect of its
/// own corners (glyph space is y-up, view space y-down, so corner order
/// flips — compare geometry, not winding).
fn approx_quad_eq(a: &[[f32; 2]; 4], b: &[[f32; 2]; 4]) -> bool {
    let bbox = |q: &[[f32; 2]; 4]| {
        q.iter()
            .fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |r, p| {
                [
                    r[0].min(p[0]),
                    r[1].min(p[1]),
                    r[2].max(p[0]),
                    r[3].max(p[1]),
                ]
            })
    };
    let (a, b) = (bbox(a), bbox(b));
    (a[0] - b[0]).abs() < 0.5
        && (a[1] - b[1]).abs() < 0.5
        && (a[2] - b[2]).abs() < 0.5
        && (a[3] - b[3]).abs() < 0.5
}

/// True when two rects (as `[x0, y0, x1, y1]`) overlap.
pub fn rects_intersect(a: &[f32; 4], b: &[f32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn quad_from_rect(r: [f32; 4]) -> [[f32; 2]; 4] {
    [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]]
}

/// Group glyphs into baseline lines: cluster by vertical center (tolerance
/// is a fraction of the median glyph height), then order each line by x.
pub fn group_lines(glyphs: Vec<TextGlyph>) -> PageText {
    let mut order: Vec<usize> = (0..glyphs.len()).collect();
    order.sort_by(|&a, &b| {
        let ay = (glyphs[a].rect[1] + glyphs[a].rect[3]) * 0.5;
        let by = (glyphs[b].rect[1] + glyphs[b].rect[3]) * 0.5;
        ay.partial_cmp(&by).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Median glyph height for the tolerance.
    let mut heights: Vec<f32> = glyphs
        .iter()
        .map(|g| (g.rect[3] - g.rect[1]).abs())
        .filter(|h| h.is_finite() && *h > 0.0)
        .collect();
    heights.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median_h = heights
        .get(heights.len() / 2)
        .copied()
        .filter(|h| h.is_finite() && *h > 0.0)
        .unwrap_or(12.0);
    let tolerance = (median_h * 0.6).max(1.0);

    let mut clusters: Vec<Vec<usize>> = Vec::new();
    for idx in order {
        let g = &glyphs[idx];
        let cy = (g.rect[1] + g.rect[3]) * 0.5;
        match clusters.last_mut() {
            Some(line) => {
                let last = &glyphs[*line.last().unwrap()];
                let lcy = (last.rect[1] + last.rect[3]) * 0.5;
                if (cy - lcy).abs() <= tolerance {
                    line.push(idx);
                } else {
                    clusters.push(vec![idx]);
                }
            }
            None => clusters.push(vec![idx]),
        }
    }

    let mut lines: Vec<TextLine> = clusters
        .into_iter()
        .map(|mut idxs| {
            idxs.sort_by(|&a, &b| {
                glyphs[a].rect[0]
                    .partial_cmp(&glyphs[b].rect[0])
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let rect = idxs.iter().fold([f32::MAX, f32::MAX, 0.0, 0.0], |acc, &i| {
                let r = &glyphs[i].rect;
                [
                    acc[0].min(r[0]),
                    acc[1].min(r[1]),
                    acc[2].max(r[2]),
                    acc[3].max(r[3]),
                ]
            });
            TextLine { rect, glyphs: idxs }
        })
        .collect();
    lines.retain(|l| !l.glyphs.is_empty());
    PageText { glyphs, lines }
}

// ---------------------------------------------------------------------------
// Glyph collector device
// ---------------------------------------------------------------------------

/// A [`Device`] that ignores paint and records every glyph's Unicode text
/// and em-box quad in page view space. Modeled on PdfCraft's text
/// extraction, subset only: no bidi, no search, no structure filters.
struct GlyphCollector {
    glyphs: Vec<TextGlyph>,
}

impl<'a> Device<'a> for GlyphCollector {
    fn set_soft_mask(&mut self, _mask: Option<SoftMask<'a>>) {}
    fn set_blend_mode(&mut self, _blend_mode: BlendMode) {}
    fn draw_path(
        &mut self,
        _path: &kurbo::BezPath,
        _transform: Affine,
        _paint: &Paint<'a>,
        _draw_mode: &PathDrawMode,
    ) {
    }
    fn push_clip_path(&mut self, _clip_path: &ClipPath) {}
    fn push_transparency_group(
        &mut self,
        _opacity: f32,
        _mask: Option<SoftMask<'a>>,
        _blend_mode: BlendMode,
    ) {
    }
    fn pop_clip_path(&mut self) {}
    fn pop_transparency_group(&mut self) {}
    fn draw_image(&mut self, _image: Image<'a, '_>, _transform: Affine) {}

    fn draw_glyph(
        &mut self,
        glyph: &hayro::hayro_interpret::font::Glyph<'a>,
        transform: Affine,
        glyph_transform: Affine,
        _paint: &Paint<'a>,
        _draw_mode: &GlyphDrawMode,
    ) {
        let text = match glyph.as_unicode() {
            Some(BfString::Char(c)) => c.to_string(),
            Some(BfString::String(s)) => s,
            None => return,
        };
        if text.chars().all(char::is_control) {
            return;
        }
        // Glyph space → view space. Glyph space uses 1000 units per em for
        // outline glyphs; the em box spans descender (−200) to ascender
        // (800), the advance gives the width.
        let t = transform * glyph_transform;
        let advance = match glyph {
            hayro::hayro_interpret::font::Glyph::Outline(g) => g
                .advance_width()
                .filter(|a| *a > 0.0)
                .map(f64::from)
                .unwrap_or_else(|| g.outline().bounding_box().width().max(500.0)),
            // Public hayro-interpret exposes no Type3 advance; a fixed
            // fallback keeps the box usable (PdfCraft uses the same value).
            hayro::hayro_interpret::font::Glyph::Type3(_) => 600.0,
        };
        // Em box: descender −200 to ascender 800 (PDF glyph units), then
        // mapped through the glyph transform into view space.
        let em = Rect::new(0.0, -200.0, advance, 800.0);
        let corners = [
            Point::new(em.x0, em.y0),
            Point::new(em.x1, em.y0),
            Point::new(em.x1, em.y1),
            Point::new(em.x0, em.y1),
        ]
        .map(|p| t * p);
        let bbox = corners.iter().fold(
            Rect::new(
                f64::INFINITY,
                f64::INFINITY,
                f64::NEG_INFINITY,
                f64::NEG_INFINITY,
            ),
            |b, p| Rect::new(b.x0.min(p.x), b.y0.min(p.y), b.x1.max(p.x), b.y1.max(p.y)),
        );
        if !(bbox.x0.is_finite()
            && bbox.y0.is_finite()
            && bbox.x1.is_finite()
            && bbox.y1.is_finite())
            || bbox.width() > 10_000.0
            || bbox.height() > 10_000.0
        {
            return;
        }
        self.glyphs.push(TextGlyph {
            text,
            rect: [
                bbox.x0 as f32,
                bbox.y0 as f32,
                bbox.x1 as f32,
                bbox.y1 as f32,
            ],
            quad: corners.map(|p| [p.x as f32, p.y as f32]),
        });
    }
}

/// Extract one page's text on the worker thread. Returns an empty
/// [`PageText`] when the interpreter panics (hostile input must never take
/// the app down).
fn extract_text(page: &Page<'_>) -> PageText {
    let device = panic::catch_unwind(AssertUnwindSafe(|| {
        let (w, h) = page.render_dimensions();
        let initial = page.initial_transform(true).to_kurbo();
        let mut collector = GlyphCollector { glyphs: Vec::new() };
        let cache = InterpreterCache::new();
        let mut context = Context::new(
            initial,
            Rect::new(0.0, 0.0, f64::from(w), f64::from(h)),
            &cache,
            page.xref(),
            InterpreterSettings::default(),
        );
        interpret_page(page, &mut context, &mut collector);
        collector.glyphs
    }));
    match device {
        Ok(glyphs) => group_lines(glyphs),
        Err(_) => PageText::default(),
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

/// One render/extract request to a document's worker.
#[derive(Debug)]
pub enum Request {
    Render { page: usize, scale: f32 },
    Text { page: usize },
    Stop,
}

/// An async result delivered to the UI thread.
#[derive(Debug)]
pub enum Event {
    /// The document parsed successfully; `page_count` follows.
    Opened {
        page_count: usize,
    },
    /// The document could not be parsed.
    OpenFailed {
        message: String,
    },
    Rendered {
        page: usize,
        scale: f32,
        width: u32,
        height: u32,
        /// Straight RGBA8 (the render runs on an opaque white background, so
        /// hayro's premultiplied output needs no conversion).
        pixels: Vec<u8>,
    },
    Text {
        page: usize,
        text: PageText,
    },
    /// A render/extract panicked or the page does not exist.
    Failed {
        page: usize,
        message: String,
    },
}

/// Handle to a document's worker thread. Dropping it sends `Stop` and joins.
pub struct PdfWorker {
    tx: Sender<Request>,
    join: Option<std::thread::JoinHandle<()>>,
}

impl PdfWorker {
    /// Spawn a worker for the PDF at `path`. Both the file read and the parse
    /// happen on the worker, so opening a large document never blocks the UI;
    /// an [`Event::Opened`] / [`Event::OpenFailed`] arrives first.
    pub fn open(path: PathBuf, repaint: egui::Context) -> (Self, Receiver<Event>) {
        let (tx, rx) = std::sync::mpsc::channel::<Request>();
        let (events_tx, events_rx) = std::sync::mpsc::channel::<Event>();
        let join = std::thread::Builder::new()
            .name("kandrium-pdf".to_string())
            .spawn(move || worker_loop(path, rx, events_tx, repaint))
            .ok();
        (Self { tx, join }, events_rx)
    }

    /// Fire-and-forget request; a dead worker is ignored (the tab shows the
    /// failure state already).
    pub fn send(&self, request: Request) {
        let _ = self.tx.send(request);
    }
}

impl Drop for PdfWorker {
    fn drop(&mut self) {
        let _ = self.tx.send(Request::Stop);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn worker_loop(
    path: PathBuf,
    rx: Receiver<Request>,
    events: Sender<Event>,
    repaint: egui::Context,
) {
    let data = match std::fs::read(&path) {
        Ok(data) => data,
        Err(e) => {
            let _ = events.send(Event::OpenFailed {
                message: format!("Cannot read PDF: {e}"),
            });
            repaint.request_repaint();
            return;
        }
    };
    let pdf = match Pdf::new(data) {
        Ok(pdf) => pdf,
        Err(e) => {
            let _ = events.send(Event::OpenFailed {
                message: format!("{e:?}"),
            });
            repaint.request_repaint();
            return;
        }
    };
    let page_count = pdf.pages().len();
    if events.send(Event::Opened { page_count }).is_err() {
        return;
    }
    repaint.request_repaint();

    let cache = hayro::RenderCache::new();
    while let Ok(request) = rx.recv() {
        match request {
            Request::Stop => break,
            Request::Render { page, scale } => {
                let event = render_page(&pdf, &cache, page, scale);
                if events.send(event).is_err() {
                    break;
                }
                repaint.request_repaint();
            }
            Request::Text { page } => {
                let Some(page_ref) = pdf.pages().get(page) else {
                    let _ = events.send(Event::Failed {
                        page,
                        message: "no such page".to_string(),
                    });
                    repaint.request_repaint();
                    continue;
                };
                let text = extract_text(page_ref);
                if events.send(Event::Text { page, text }).is_err() {
                    break;
                }
                repaint.request_repaint();
            }
        }
    }
}

/// Render one page at `scale` on an opaque white background and hand back
/// straight RGBA8 pixels.
fn render_page<'a>(pdf: &'a Pdf, cache: &hayro::RenderCache<'a>, page: usize, scale: f32) -> Event {
    let Some(page_ref) = pdf.pages().get(page) else {
        return Event::Failed {
            page,
            message: "no such page".to_string(),
        };
    };
    let settings = hayro::RenderSettings {
        x_scale: scale,
        y_scale: scale,
        bg_color: hayro::vello_cpu::color::AlphaColor::from_rgba8(255, 255, 255, 255),
        ..hayro::RenderSettings::default()
    };
    let rendered = panic::catch_unwind(AssertUnwindSafe(|| {
        hayro::render(page_ref, cache, &InterpreterSettings::default(), &settings)
    }));
    match rendered {
        Ok(pixmap) => {
            let (w, h) = (pixmap.width() as u32, pixmap.height() as u32);
            let mut pixels = pixmap.data_as_u8_slice().to_vec();
            for px in pixels.as_chunks_mut::<4>().0 {
                // vello's pixmap is premultiplied RGBA8; the opaque white
                // background guarantees alpha = 255 everywhere, so premultiplied
                // == straight — but clamp defensively anyway.
                px[3] = 255;
            }
            Event::Rendered {
                page,
                scale,
                width: w,
                height: h,
                pixels,
            }
        }
        Err(_) => Event::Failed {
            page,
            message: "the page could not be rendered".to_string(),
        },
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// End-to-end smoke test: build a minimal one-page PDF with real text
    /// (Helvetica via the `embed-fonts` feature), render it, and extract
    /// its glyphs. Fully self-contained — no sample files needed.
    #[test]
    fn worker_renders_and_extracts_real_pdf() {
        let data = minimal_text_pdf("Hello Kandrium");
        let pdf = Pdf::new(data).expect("parse");
        assert_eq!(pdf.pages().len(), 1);
        let cache = hayro::RenderCache::new();
        let event = render_page(&pdf, &cache, 0, 2.0);
        match event {
            Event::Rendered { width, height, .. } => {
                let (pw, ph) = pdf.pages()[0].render_dimensions();
                assert_eq!(width as f32, (pw * 2.0).round());
                assert_eq!(height as f32, (ph * 2.0).round());
                assert!(width > 10 && height > 10, "tiny render: {width}x{height}");
            }
            Event::Failed { message, .. } => panic!("render failed: {message}"),
            other => panic!("unexpected event: {other:?}"),
        }
        let text = extract_text(&pdf.pages()[0]);
        assert!(!text.glyphs.is_empty(), "no glyphs extracted");
        assert!(!text.lines.is_empty(), "no lines grouped");
        // The extracted text must contain our string (glyphs in x order).
        let joined: String = text
            .line_glyphs(&text.lines[0])
            .map(|g| g.text.as_str())
            .collect();
        assert!(joined.contains("Hello"), "first line reads: {joined:?}");
        // Glyph rects must be sane (positive size).
        for g in &text.glyphs {
            assert!(g.rect[2] >= g.rect[0] && g.rect[3] >= g.rect[1]);
        }
        // selection_quads over the whole page hits the line.
        let (pw, ph) = pdf.pages()[0].render_dimensions();
        let quads = text.selection_quads(&[0.0, 0.0, pw, ph]);
        assert_eq!(quads.len(), 1);
    }

    /// Build a minimal valid single-page PDF containing `s` drawn at
    /// 24 pt Helvetica, with a correct xref table.
    fn minimal_text_pdf(s: &str) -> Vec<u8> {
        let content = format!("BT /F1 24 Tf 72 700 Td ({s}) Tj ET\n");
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << /Font << /F1 5 0 R >> >> >>".to_string(),
            format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
        ];
        let mut out = Vec::from(&b"%PDF-1.4\n"[..]);
        let mut offsets = Vec::new();
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, body).as_bytes());
        }
        let xref_at = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                objects.len() + 1,
                xref_at
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn group_lines_clusters_by_baseline() {
        let mk = |x: f32, y: f32, text: &str| TextGlyph {
            text: text.to_string(),
            rect: [x, y, x + 6.0, y + 10.0],
            quad: [[x, y], [x + 6.0, y], [x + 6.0, y + 10.0], [x, y + 10.0]],
        };
        // Two rows of three glyphs; second row starts further right.
        let glyphs = vec![
            mk(10.0, 100.0, "a"),
            mk(20.0, 101.0, "b"),
            mk(30.0, 100.0, "c"),
            mk(10.0, 130.0, "d"),
            mk(20.0, 131.0, "e"),
        ];
        let page = group_lines(glyphs);
        assert_eq!(page.lines.len(), 2);
        assert_eq!(page.lines[0].glyphs, vec![0, 1, 2]);
        assert_eq!(page.lines[1].glyphs, vec![3, 4]);
        // Reading order within a line: sorted by x.
        let joined: String = page
            .line_glyphs(&page.lines[0])
            .map(|g| g.text.as_str())
            .collect();
        assert_eq!(joined, "abc");
    }

    #[test]
    fn selection_quads_merges_per_line() {
        let mk = |x: f32, y: f32| TextGlyph {
            text: "x".to_string(),
            rect: [x, y, x + 6.0, y + 10.0],
            quad: [[x, y], [x + 6.0, y], [x + 6.0, y + 10.0], [x, y + 10.0]],
        };
        let page = group_lines(vec![mk(10.0, 0.0), mk(20.0, 0.0), mk(10.0, 30.0)]);
        // Selection covering only the first line's left glyph.
        let quads = page.selection_quads(&[0.0, 0.0, 15.0, 20.0]);
        assert_eq!(quads.len(), 1);
        assert!((quads[0][0][0] - 10.0).abs() < 1e-4);
        // Selection covering both lines.
        let quads = page.selection_quads(&[0.0, 0.0, 100.0, 100.0]);
        assert_eq!(quads.len(), 2);
        // Empty selection.
        assert!(
            page.selection_quads(&[500.0, 500.0, 501.0, 501.0])
                .is_empty()
        );
    }

    #[test]
    fn rects_intersect_detects_overlap() {
        assert!(rects_intersect(
            &[0.0, 0.0, 10.0, 10.0],
            &[5.0, 5.0, 20.0, 20.0]
        ));
        assert!(!rects_intersect(
            &[0.0, 0.0, 10.0, 10.0],
            &[11.0, 0.0, 20.0, 10.0]
        ));
    }
}
