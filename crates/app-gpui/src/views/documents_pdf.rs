//! The PDF viewer: toolbar, page stack, zoom, page navigation, selection and
//! the annotation overlay.
//!
//! Rendering runs in `pdf_render::PdfWorker` (one thread per tab), exactly as
//! in the egui app. Two gpui specifics drive the code:
//!
//! - `RenderImage` takes **BGRA** rows and has no `From<RgbaImage>`, so the
//!   worker's RGBA frame is byte-swapped when the event is applied.
//! - There is no transform/scale style: zoom is *sizing*. Pages are rendered
//!   at `fit × zoom` and laid out at `page points × that scale`.
//!
//! gpui has no cross-thread repaint either, so requests sent while rendering
//! set `DocumentsState::needs_repaint` and the shell keeps painting until the
//! worker catches up.

use gpui_kit::component::input::Textarea;
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, ClickEvent, Context, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Rgba, TestSupportExt, Window, canvas, div, img, px, rgb,
};

use kandrium_core::documents::{AnnotationKind, DocAnnotation};

use crate::app::AppShell;
use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens};
use crate::views::documents::{
    self, DocTab, DocumentsState, HIGHLIGHT_ALPHA, NOTE_ANCHOR, NOTE_COLORS, PAGE_MARGIN,
    VIEWER_RIGHT_INSET, ZOOM_MIN, color_with_alpha, permille, visible_pages,
};
use pdf_render::Request;

/// The small slice of the tab the toolbar needs, so the toolbar can render
/// while the rest of the viewer mutates the tab.
struct Header {
    path: std::path::PathBuf,
    page_count: usize,
    visible_page: usize,
    zoom: f32,
}

/// Render the active PDF tab.
pub fn viewer(
    state: &mut DocumentsState,
    index: usize,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    debug_assert_eq!(state.tabs[index].kind, documents::DocKind::Pdf);
    let header = Header {
        path: state.tabs[index].path.clone(),
        page_count: state.tabs[index].page_count,
        visible_page: state.tabs[index].visible_page,
        zoom: state.tabs[index].zoom,
    };
    let tool = state.tool;

    let column = div()
        .id("doc-pdf")
        .test_support()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(toolbar(&header, tool, t, cx));

    if let Some(error) = state.tabs[index].error.clone() {
        return column
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .p(px(24.))
                    .child(
                        div()
                            .max_w(px(460.))
                            .text(Text::Small)
                            .text_color(t.text_muted)
                            .child(error),
                    ),
            )
            .into_any_element();
    }

    if state.tabs[index].page_count == 0 {
        // The worker answers `Opened` first; keep frames coming until it does.
        state.needs_repaint = true;
        return column
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text(Text::Small)
                    .text_color(t.text_muted)
                    .child("Loading…"),
            )
            .into_any_element();
    }

    let viewport = *state.tabs[index].viewport.borrow();
    let viewport_w = viewport
        .map(|bounds| bounds.size.width.as_f32())
        .filter(|w| *w > 0.0)
        .unwrap_or(800.0);
    let viewport_h = viewport
        .map(|bounds| bounds.size.height.as_f32())
        .filter(|h| *h > 0.0)
        .unwrap_or(600.0);

    let Some((page_w_pt, page_h_pt)) = state.tabs[index].page_size else {
        // Learn the page size from a 1:1 render of the first page.
        let page_count = state.tabs[index].page_count;
        if request(&mut state.tabs[index], 0, 1.0, page_count) {
            state.needs_repaint = true;
        }
        return column
            .child(
                div()
                    .flex_1()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text(Text::Small)
                    .text_color(t.text_muted)
                    .child("Preparing pages…"),
            )
            .into_any_element();
    };

    let fit = ((viewport_w - VIEWER_RIGHT_INSET - 2.0 * PAGE_MARGIN) / page_w_pt).clamp(0.1, 8.0);
    let scale = fit * state.tabs[index].zoom;
    state.tabs[index].last_scale = scale;
    let page_w = page_w_pt * scale;
    let page_h = page_h_pt * scale;
    let row_h = page_h + 2.0 * PAGE_MARGIN;

    if let Some(target) = state.tabs[index].jump_to.take() {
        state.tabs[index].scroll.scroll_to_top_of_item(target);
    }
    let top = (-state.tabs[index].scroll.offset().y.as_f32()).max(0.0);
    let count = state.tabs[index].page_count;
    let visible = visible_pages(top, viewport_h.max(1.0), row_h, count);
    if let Some(first) = visible.first() {
        state.tabs[index].visible_page = *first;
    }

    let mut sent = false;
    for page in &visible {
        sent |= request(&mut state.tabs[index], *page, scale, count);
    }
    if sent {
        state.needs_repaint = true;
    }

    // The stack sits in a relative wrapper so the viewport-recording canvas is
    // a sibling, not the first child of the scroll container (page jumps rely
    // on page index == child index).
    let viewport_handle = state.tabs[index].viewport.clone();
    let mut stack = div()
        .id("doc-pdf-pages")
        .test_support()
        .flex_1()
        .min_h_0()
        .overflow_scroll()
        .track_scroll(&state.tabs[index].scroll);
    for page in 0..count {
        stack = stack.child(page_row(state, index, page, scale, (page_w, page_h), t, cx));
    }

    let track = div()
        .id("doc-pdf-track")
        .test_support()
        .relative()
        .flex_1()
        .min_h_0()
        .child(
            canvas(
                move |bounds, _window, _cx| {
                    *viewport_handle.borrow_mut() = Some(bounds);
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full(),
        )
        .child(stack);

    column.child(track).into_any_element()
}

fn toolbar(
    header: &Header,
    tool: documents::Tool,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let ready = header.page_count > 0;
    let prev_path = header.path.clone();
    let next_path = header.path.clone();
    let out_path = header.path.clone();
    let reset_path = header.path.clone();
    let in_path = header.path.clone();

    let mut prev = div()
        .id("doc-pdf-prev")
        .test_support()
        .p(px(3.))
        .rounded(px(4.))
        .child(icon(Icon::ChevronLeft, 13., t.text_muted));
    if ready {
        prev = prev
            .cursor_pointer()
            .hover(|style| style.bg(t.hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.jump_doc_page(prev_path.clone(), -1, cx)
            }));
    }
    let mut next = div()
        .id("doc-pdf-next")
        .test_support()
        .p(px(3.))
        .rounded(px(4.))
        .child(icon(Icon::ChevronRight, 13., t.text_muted));
    if ready {
        next = next
            .cursor_pointer()
            .hover(|style| style.bg(t.hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.jump_doc_page(next_path.clone(), 1, cx)
            }));
    }

    let mut bar = div()
        .id("doc-pdf-toolbar")
        .test_support()
        .flex()
        .items_center()
        .gap(px(6.))
        .px(px(12.))
        .py(px(5.))
        .border_b_1()
        .border_color(t.divider)
        .child(prev)
        .child(
            div()
                .text(Text::Small)
                .text_color(t.text_muted)
                .child(format!(
                    "{} / {}",
                    (header.visible_page + 1).min(header.page_count.max(1)),
                    header.page_count
                )),
        )
        .child(next)
        .child(div().w(px(10.)))
        .child(tool_button(
            "doc-pdf-zoom-out",
            Icon::Minus,
            t.text_muted,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.zoom_doc_by(out_path.clone(), -documents::ZOOM_STEP, cx)
            }),
        ))
        .child(
            div()
                .id("doc-pdf-zoom-label")
                .test_support()
                .px(px(6.))
                .py(px(2.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|style| style.bg(t.hover))
                .text(Text::Small)
                .text_color(t.text)
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.reset_doc_zoom(reset_path.clone(), cx)
                }))
                .child(format!("{:.0}%", header.zoom * 100.0)),
        )
        .child(tool_button(
            "doc-pdf-zoom-in",
            Icon::Plus,
            t.text_muted,
            cx.listener(move |this, _: &ClickEvent, _window, cx| {
                this.zoom_doc_by(in_path.clone(), documents::ZOOM_STEP, cx)
            }),
        ))
        .child(div().w(px(10.)));

    for entry in [
        documents::Tool::Select,
        documents::Tool::Highlight,
        documents::Tool::Note,
    ] {
        let active = tool == entry;
        bar = bar.child(
            div()
                .id(format!("doc-pdf-tool-{}", entry.label()))
                .test_support()
                .flex()
                .items_center()
                .gap(px(4.))
                .px(px(8.))
                .py(px(2.))
                .rounded(px(t.radius))
                .cursor_pointer()
                .when(active, |this| this.bg(t.accent_soft))
                .hover(|style| style.bg(t.hover))
                .text(Text::Small)
                .text_color(if active { t.accent_text } else { t.text_muted })
                .child(icon(
                    entry.icon(),
                    12.,
                    if active { t.accent_text } else { t.text_muted },
                ))
                .child(entry.label())
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.set_doc_tool(entry, cx)
                })),
        );
    }

    bar.into_any_element()
}

fn page_row(
    state: &mut DocumentsState,
    index: usize,
    page: usize,
    scale: f32,
    page_size: (f32, f32),
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let tab = &state.tabs[index];
    let image = tab.images.get(&(page, permille(scale))).cloned();
    let page_w = page_size.0;
    let page_h = page_size.1;
    let bounds_handle = tab.page_bounds.clone();
    let selection = tab.selection.filter(|sel| sel.page == page);

    let mut body = div()
        .id(format!("doc-page-body-{page}"))
        .relative()
        .w(px(page_w))
        .h(px(page_h))
        .bg(rgb(0xffffff))
        .border_1()
        .border_color(t.border);

    let recorded = bounds_handle.clone();
    body = body.child(
        canvas(
            move |bounds, _window, _cx| {
                recorded.borrow_mut().insert(page, bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full(),
    );

    match image {
        Some(image) => {
            body = body.child(
                div()
                    .absolute()
                    .size_full()
                    .child(img(image).w(px(page_w)).h(px(page_h))),
            );
        }
        None => {
            body = body.child(
                div()
                    .absolute()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text(Text::Small)
                    .text_color(rgb(0x8a8f98))
                    .child("Rendering…"),
            );
        }
    }

    body = body.children(overlays(state, index, page, scale, t, cx));

    if let Some(selection) = selection {
        let r = selection.rect();
        body = body.child(
            div()
                .absolute()
                .left(px(r[0] * scale))
                .top(px(r[1] * scale))
                .w(px((r[2] - r[0]) * scale))
                .h(px((r[3] - r[1]) * scale))
                .bg(color_with_alpha("#8ab4f8", 64)),
        );
    }

    let path = state.tabs[index].path.clone();
    let down = bounds_handle.clone();
    let moved = bounds_handle.clone();
    let up = bounds_handle.clone();
    let down_path = path.clone();
    let move_path = path.clone();
    let up_path = path;

    body = body
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                let origin = down.borrow().get(&page).map(|b| b.origin);
                this.doc_selection_down(
                    down_path.clone(),
                    page,
                    origin,
                    event.position,
                    window,
                    cx,
                );
            }),
        )
        .on_mouse_move(
            cx.listener(move |this, event: &MouseMoveEvent, _window, cx| {
                if event.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                let origin = moved.borrow().get(&page).map(|b| b.origin);
                this.doc_selection_move(move_path.clone(), page, origin, event.position, cx);
            }),
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseUpEvent, window, cx| {
                let origin = up.borrow().get(&page).map(|b| b.origin);
                this.doc_selection_up(up_path.clone(), page, origin, event.position, window, cx);
            }),
        );

    div()
        .id(format!("doc-page-{page}"))
        .test_support()
        .w_full()
        .flex_shrink_0()
        .flex()
        .justify_center()
        .py(px(PAGE_MARGIN))
        .child(body)
        .into_any_element()
}

/// Highlight fills and note anchors for one page, plus the open popover.
fn overlays(
    state: &DocumentsState,
    index: usize,
    page: usize,
    scale: f32,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> Vec<AnyElement> {
    let tab = &state.tabs[index];
    let mut out = Vec::new();
    for ann in tab.annotations.iter().filter(|ann| ann.page == page) {
        let open = tab.popover.as_deref() == Some(ann.id.as_str());
        let path = tab.path.clone();
        let id = ann.id.clone();
        match ann.kind {
            AnnotationKind::Highlight => {
                for (quad_index, quad) in ann.quads.iter().enumerate() {
                    let r = quad_bounds(quad, scale);
                    out.push(
                        div()
                            .id(format!("doc-ann-{}-{quad_index}", ann.id))
                            .absolute()
                            .left(px(r[0]))
                            .top(px(r[1]))
                            .w(px((r[2] - r[0]).max(1.0)))
                            .h(px((r[3] - r[1]).max(1.0)))
                            .rounded(px(1.))
                            .bg(color_with_alpha(&ann.color, HIGHLIGHT_ALPHA))
                            .when(open, |this| this.border_1().border_color(t.accent))
                            .cursor_pointer()
                            .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _window, cx| {
                                cx.stop_propagation();
                            })
                            .on_mouse_up(MouseButton::Left, {
                                let path = path.clone();
                                let id = id.clone();
                                cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                                    cx.stop_propagation();
                                    this.open_annotation(path.clone(), id.clone(), window, cx);
                                })
                            })
                            .into_any_element(),
                    );
                }
            }
            AnnotationKind::Note => {
                let Some(quad) = ann.quads.first() else {
                    continue;
                };
                let r = quad_bounds(quad, scale);
                out.push(
                    div()
                        .id(format!("doc-ann-{}", ann.id))
                        .absolute()
                        .left(px(r[0]))
                        .top(px(r[1]))
                        .w(px((r[2] - r[0]).max(NOTE_ANCHOR * scale)))
                        .h(px((r[3] - r[1]).max(NOTE_ANCHOR * scale)))
                        .rounded(px(3.))
                        .bg(color_with_alpha(&ann.color, 235))
                        .border_1()
                        .border_color(if open { t.accent } else { t.border })
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .child(icon(Icon::StickyNote, 11., rgb(0x2b2b2b)))
                        .on_mouse_down(MouseButton::Left, |_: &MouseDownEvent, _window, cx| {
                            cx.stop_propagation();
                        })
                        .on_mouse_up(MouseButton::Left, {
                            let path = path.clone();
                            let id = id.clone();
                            cx.listener(move |this, _: &MouseUpEvent, window, cx| {
                                cx.stop_propagation();
                                this.open_annotation(path.clone(), id.clone(), window, cx);
                            })
                        })
                        .into_any_element(),
                );
            }
        }
    }

    if let Some(ann) = tab
        .popover
        .as_ref()
        .and_then(|id| tab.annotations.iter().find(|ann| &ann.id == id))
        .filter(|ann| ann.page == page)
    {
        out.push(popover(tab, ann, scale, t, cx));
    }
    out
}

fn quad_bounds(quad: &[[f32; 2]; 4], scale: f32) -> [f32; 4] {
    let x0 = quad.iter().fold(f32::MAX, |a, p| a.min(p[0])) * scale;
    let y0 = quad.iter().fold(f32::MAX, |a, p| a.min(p[1])) * scale;
    let x1 = quad.iter().fold(f32::MIN, |a, p| a.max(p[0])) * scale;
    let y1 = quad.iter().fold(f32::MIN, |a, p| a.max(p[1])) * scale;
    [x0, y0, x1, y1]
}

/// Memo, colours, delete and close for one annotation.
fn popover(
    tab: &DocTab,
    ann: &DocAnnotation,
    scale: f32,
    t: &Tokens,
    cx: &mut Context<AppShell>,
) -> AnyElement {
    let quad = ann.quads.first().copied().unwrap_or([[0.0, 0.0]; 4]);
    let r = quad_bounds(&quad, scale);
    let left = r[0].clamp(4.0, 380.0);
    let top = (r[3] + 6.0).max(4.0);
    let path = tab.path.clone();
    let close_path = tab.path.clone();
    let delete_path = tab.path.clone();
    let delete_id = ann.id.clone();

    let heading = match ann.kind {
        AnnotationKind::Highlight => "Highlight",
        AnnotationKind::Note => "Note",
    };
    let mut card = div()
        .id("doc-annotation-popover")
        .test_support()
        .absolute()
        .left(px(left))
        .top(px(top))
        .w(px(260.))
        .p(px(8.))
        .rounded(px(t.radius))
        .bg(t.card)
        .border_1()
        .border_color(t.border)
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(6.))
                .child(
                    div()
                        .flex_1()
                        .text(Text::SmallStrong)
                        .text_color(t.text)
                        .child(heading),
                )
                .child(
                    div()
                        .id("doc-annotation-close")
                        .p(px(2.))
                        .rounded(px(3.))
                        .cursor_pointer()
                        .hover(|style| style.bg(t.hover))
                        .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                            cx.stop_propagation();
                            this.close_doc_popover(close_path.clone(), cx)
                        }))
                        .child(icon(Icon::X, 11., t.text_faint)),
                ),
        );

    if let Some(editor) = &tab.popover_editor {
        card = card.child(div().min_h(px(56.)).child(Textarea::new(editor)));
    }

    let mut colors = div().flex().items_center().gap(px(6.));
    for color in NOTE_COLORS {
        let swatch_path = path.clone();
        let id = ann.id.clone();
        let color_owned = color.to_string();
        colors = colors.child(
            div()
                .id(format!("doc-ann-color-{color}"))
                .size(px(14.))
                .rounded(px(7.))
                .bg(color_with_alpha(color, 255))
                .border_1()
                .border_color(if ann.color == color {
                    t.accent
                } else {
                    t.border
                })
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.set_annotation_color(
                        swatch_path.clone(),
                        id.clone(),
                        color_owned.clone(),
                        cx,
                    )
                })),
        );
    }
    card = card.child(
        colors.child(div().flex_1()).child(
            div()
                .id("doc-annotation-delete")
                .px(px(6.))
                .py(px(2.))
                .rounded(px(4.))
                .cursor_pointer()
                .hover(|style| style.bg(t.hover))
                .text(Text::Small)
                .text_color(t.danger)
                .on_click(cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    cx.stop_propagation();
                    this.delete_annotation(delete_path.clone(), delete_id.clone(), cx)
                }))
                .child("Delete"),
        ),
    );

    card.into_any_element()
}

fn tool_button(
    id: &'static str,
    glyph: Icon,
    color: Rgba,
    listener: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    div()
        .id(id)
        .test_support()
        .p(px(3.))
        .rounded(px(4.))
        .cursor_pointer()
        .hover(|style| style.bg(color_with_alpha("#ffffff", 18)))
        .on_click(listener)
        .child(icon(glyph, 13., color))
        .into_any_element()
}

/// Ask the worker for a page texture when the cache has no entry for this
/// scale, and for the page's text once. Returns true when a request went out,
/// so the shell keeps repainting until the event arrives.
fn request(tab: &mut DocTab, page: usize, scale: f32, page_count: usize) -> bool {
    let mut sent = false;
    if page < page_count && !tab.images.contains_key(&(page, permille(scale))) {
        let key = (page, permille(scale));
        if tab.pending.insert(key) {
            if let Some(worker) = &tab.worker {
                worker.send(Request::Render { page, scale });
            }
            sent = true;
        }
    }
    if !tab.text_requested.contains(&page) {
        tab.text_requested.insert(page);
        if let Some(worker) = &tab.worker {
            worker.send(Request::Text { page });
        }
        sent = true;
    }
    sent
}

/// Zoom bounds, used by both the toolbar and the shell.
pub fn clamp_zoom(zoom: f32) -> f32 {
    zoom.clamp(ZOOM_MIN, documents::ZOOM_MAX)
}
