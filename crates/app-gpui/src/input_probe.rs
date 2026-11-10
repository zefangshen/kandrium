//! B. Text input: focus, read the caret, move it programmatically, and prove
//! `Esc` reaches an outer handler **without** the field losing focus.
//!
//! This is the load-bearing question for the vim port: if `Esc` blurred the
//! field, the modal layer would have to re-implement input handling from
//! scratch. Everything printed here is the evidence.

use std::time::Duration;

use gpui_kit::component::input::{Input, InputState, Position};
use gpui_kit::*;

pub fn run() {
    application().run(|cx| {
        gpui_kit::init(cx);
        open_window(WindowOptions::default(), cx, |window, cx| {
            let state = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder("type here, then press Esc")
                    .default_value("abcdefghij")
            });

            state.update(cx, |state, cx| {
                println!("[B] value={:?}", state.value());
                println!("[B] cursor before: {:?}", state.cursor_position());
                state.set_cursor_position(
                    Position {
                        line: 0,
                        character: 5,
                    },
                    window,
                    cx,
                );
                println!(
                    "[B] cursor after set_cursor_position(0,5): {:?}",
                    state.cursor_position()
                );
                state.set_selected_range(0..4, cx);
                println!(
                    "[B] selected_range after set_selected_range(0..4): {:?}",
                    state.selected_range()
                );
                state.focus(window, cx);
                println!("[B] focused={}", state.focus_handle(cx).is_focused(window));
            });

            cx.new(|_| InputProbe {
                state,
                last_esc: None,
                probe_sent: false,
            })
        })
        .expect("open input window");
    });
}

struct InputProbe {
    state: Entity<InputState>,
    last_esc: Option<(String, bool)>,
    probe_sent: bool,
}

impl Render for InputProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.probe_sent {
            self.probe_sent = true;
            // Dispatch a real Escape through the keymap once the window is up,
            // then report whether the field kept focus. This is the same code
            // path a physical key press takes.
            let state = self.state.clone();
            cx.spawn_in(window, async move |_this, cx| {
                cx.background_executor()
                    .timer(Duration::from_millis(1200))
                    .await;
                cx.update(|window, cx| {
                    let before = state.read(cx).focus_handle(cx).is_focused(window);
                    let handled = window.dispatch_keystroke(
                        Keystroke::parse("escape").expect("escape keystroke"),
                        cx,
                    );
                    let after = state.read(cx).focus_handle(cx).is_focused(window);
                    let caret = state.read(cx).cursor_position();
                    println!(
                        "[B] synthetic Escape: keymap_handled={handled} focus_before={before} focus_after={after} caret={caret:?}"
                    );
                })
                .ok();
            })
            .detach();
        }

        let focused = self.state.read(cx).focus_handle(cx).is_focused(window);
        let caret = self.state.read(cx).cursor_position();
        let esc = self
            .last_esc
            .clone()
            .map(|(when, focused)| format!("{when} (focused: {focused})"))
            .unwrap_or_else(|| "none yet".to_string());

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1f22))
            .text_color(rgb(0xd9d9d9))
            .p_6()
            .gap_3()
            .child(div().text_xl().child("B · text input probe"))
            .child(
                div()
                    .text_sm()
                    .text_color(rgb(0x9aa0a6))
                    .child(format!("focused={focused}  caret={caret:?}")),
            )
            .child(Input::new(&self.state))
            .child(
                div()
                    .text_sm()
                    .child("Esc is logged to stdout; the field must keep focus."),
            )
            .child(div().text_sm().child(format!("last Esc: {esc}")))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key != "escape" {
                    return;
                }
                let focused = this.state.read(cx).focus_handle(cx).is_focused(window);
                // Prove the caret is still readable after the key arrives.
                let caret = this.state.read(cx).cursor_position();
                println!("[B] key_down escape: focused={focused} caret={caret:?}");
                this.last_esc = Some(("escape".to_string(), focused));
                cx.notify();
            }))
    }
}
