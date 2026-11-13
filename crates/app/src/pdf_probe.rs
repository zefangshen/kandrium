//! C. PDF surface: reuse the shared `pdf-render` worker, hand its RGBA frame
//! to gpui as an image, and zoom/pan it with plain layout.
//!
//! The renderer is *not* reimplemented: `pdf-render` is the same crate the
//! egui app uses. The only new code is the RGBA -> BGRA conversion gpui's
//! `RenderImage` requires and the view transform.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gpui_kit::*;

use pdf_render::{Event, PdfWorker, Request};

pub fn run() {
    let path = find_pdf();
    println!("[C] pdf: {}", path.display());

    let (worker, rx) = PdfWorker::open(path, || {});
    worker.send(Request::Render {
        page: 0,
        scale: 2.0,
    });

    let (width, height, image) = loop {
        match rx.recv_timeout(Duration::from_secs(20)) {
            Ok(Event::Rendered {
                page,
                scale,
                width,
                height,
                mut pixels,
            }) => {
                println!(
                    "[C] rendered page {page} at scale {scale}: {width}x{height}, {} bytes",
                    pixels.len()
                );
                // gpui's RenderImage is BGRA on little-endian; the worker
                // produces straight RGBA8.
                for px in pixels.as_chunks_mut::<4>().0 {
                    px.swap(0, 2);
                }
                let frame = image::Frame::new(
                    image::RgbaImage::from_raw(width, height, pixels).expect("rgba buffer"),
                );
                break (width, height, RenderImage::new(smallvec::smallvec![frame]));
            }
            Ok(other) => println!("[C] event: {other:?}"),
            Err(error) => panic!("no Rendered event within 20s: {error}"),
        }
    };
    println!("[C] RenderImage {}x{}", width, height);

    let image = Arc::new(image);
    application().run(move |cx| {
        gpui_kit::init(cx);
        let image = image.clone();
        open_window(WindowOptions::default(), cx, move |_window, cx| {
            cx.new(|_| PdfView {
                image: image.clone(),
                width: width as f32,
                height: height as f32,
                zoom: 0.5,
                offset: point(px(0.0), px(0.0)),
                last_drag: None,
                printed: false,
            })
        })
        .expect("open pdf window");
    });
}

fn find_pdf() -> PathBuf {
    if let Ok(path) = std::env::var("SPIKE_PDF") {
        return PathBuf::from(path);
    }
    let root = std::env::var("KANDRIUM_DOCS")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            kandrium_core::resolve_db_path()
                .parent()
                .expect("db has a parent")
                .join("documents")
        });
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("pdf"))
            {
                return path;
            }
        }
    }
    panic!("no PDF under {}; set SPIKE_PDF", root.display());
}

struct PdfView {
    image: Arc<RenderImage>,
    width: f32,
    height: f32,
    zoom: f32,
    offset: Point<Pixels>,
    last_drag: Option<Point<Pixels>>,
    printed: bool,
}

impl Render for PdfView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.printed {
            self.printed = true;
            println!(
                "[C] first frame built: image {}x{} at zoom {}",
                self.width, self.height, self.zoom
            );
        }
        let width = px(self.width * self.zoom);
        let height = px(self.height * self.zoom);
        div()
            .size_full()
            .bg(rgb(0x101114))
            .overflow_hidden()
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _window, cx| {
                let dy = match event.delta {
                    ScrollDelta::Pixels(delta) => delta.y.as_f32(),
                    ScrollDelta::Lines(delta) => delta.y * 24.0,
                };
                this.zoom = (this.zoom * (1.0 + dy * 0.0015)).clamp(0.1, 6.0);
                println!("[C] zoom {:.2}", this.zoom);
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.last_drag = Some(event.position);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _window, cx| {
                if let (Some(last), Some(_pressed)) = (this.last_drag, event.pressed_button) {
                    this.offset += event.position - last;
                    this.last_drag = Some(event.position);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.last_drag = None;
                    cx.notify();
                }),
            )
            .child(
                div()
                    .absolute()
                    .left(self.offset.x)
                    .top(self.offset.y)
                    .child(img(self.image.clone()).w(width).h(height)),
            )
    }
}
