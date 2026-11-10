//! The one real test the spike owes: boot a window under `TestAppContext`,
//! draw a frame, and observe layout, a key event and a click.
//!
//! Run with `cargo test -p kandrium-app-gpui`. No window is shown and no GPU is
//! touched — the test platform substitutes for both.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Context, FocusHandle, Focusable, InteractiveElement, IntoElement, ParentElement, Render,
    StatefulInteractiveElement, Styled, TestAppContext, Window, div, px, rgb,
};
use gpui_kit::base::TestSupportExt;
use gpui_kit::test::TestWindowExt;

struct Probe {
    focus: FocusHandle,
    renders: Rc<Cell<usize>>,
    escapes: Rc<Cell<usize>>,
    clicks: Rc<Cell<usize>>,
}

impl Focusable for Probe {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for Probe {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        let escapes = self.escapes.clone();
        let clicks = self.clicks.clone();
        div()
            .size_full()
            .bg(rgb(0x1e1f22))
            .track_focus(&self.focus)
            .on_key_down(move |_event, _window, _cx| escapes.set(escapes.get() + 1))
            .child(
                div()
                    .id("probe-card")
                    .test_support()
                    .w(px(160.))
                    .h(px(48.))
                    .bg(rgb(0x2f3136))
                    .child("hello"),
            )
            .child(
                div()
                    .id("probe-button")
                    .test_support()
                    .w(px(80.))
                    .h(px(24.))
                    .bg(rgb(0x3a3d43))
                    .child("click")
                    .on_click(move |_event, _window, _cx| clicks.set(clicks.get() + 1)),
            )
    }
}

#[gpui::test]
fn window_draws_and_delivers_input(cx: &mut TestAppContext) {
    let renders = Rc::new(Cell::new(0));
    let escapes = Rc::new(Cell::new(0));
    let clicks = Rc::new(Cell::new(0));

    let (view, cx) = cx.add_window_view(|_window, cx| Probe {
        focus: cx.focus_handle(),
        renders: renders.clone(),
        escapes: escapes.clone(),
        clicks: clicks.clone(),
    });

    // One explicit frame: focus the root, then draw and observe layout.
    let focus = cx.read(|app| view.read(app).focus.clone());
    let before = cx.read(|app| view.read(app).renders.get());
    let drawn = cx.update(|window, cx| {
        focus.focus(window, cx);
        window.render_frame(cx);
        view.read(cx).renders.get()
    });

    assert!(
        drawn > before,
        "an explicit draw did not re-render the root view (before {before}, after {drawn})"
    );

    // gpui-kit's snapshot layer sees the element the view built.
    let bounds = cx.update(|window, _cx| window.find("probe-card").bounds());
    assert_eq!(bounds.size.width, px(160.), "unexpected card width");

    // A synthesized click reaches the handler on the child element.
    cx.update(|window, cx| window.click("probe-button", cx));
    assert_eq!(clicks.get(), 1, "click never reached the element's handler");

    // A synthesized key reaches the focused root and does not blur it.
    cx.simulate_keystrokes("escape");
    assert_eq!(escapes.get(), 1, "escape never reached the key handler");
    assert!(
        cx.update(|window, cx| view.read(cx).focus.is_focused(window)),
        "the focused view lost focus after escape"
    );
}
