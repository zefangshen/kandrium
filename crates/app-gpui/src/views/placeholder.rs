//! Shared empty, error and "not ported yet" states.

use gpui_kit::TestSupportExt;
use gpui_kit::prelude::*;
use gpui_kit::{IntoElement, SharedString, div, px};

use crate::assets::{Icon, icon};
use crate::theme::{Text, TextStyled, Tokens};

/// A centred card explaining why a section has no content.
pub fn message(
    id: &'static str,
    glyph: Icon,
    title: impl Into<SharedString>,
    body: impl Into<SharedString>,
    t: &Tokens,
) -> impl IntoElement {
    div()
        .id(id)
        .test_support()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.))
        .child(
            div()
                .w(px(460.))
                .flex()
                .flex_col()
                .gap(px(8.))
                .rounded(px(t.radius))
                .bg(t.card)
                .border_1()
                .border_color(t.border)
                .p(px(20.))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(icon(glyph, 16., t.text_muted))
                        .child(
                            div()
                                .text(Text::Strong)
                                .text_color(t.text)
                                .child(title.into()),
                        ),
                )
                .child(
                    div()
                        .text(Text::Body)
                        .text_color(t.text_muted)
                        .child(body.into()),
                ),
        )
}
