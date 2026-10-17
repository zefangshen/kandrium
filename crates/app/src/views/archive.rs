//! Archived-cards view: search, board filter, sortable table, restore,
//! permanent delete.

use egui::{
    Align, Color32, Layout, Rect, RichText, Sense, Shape, Stroke, TextStyle, Ui, pos2, vec2,
};

use egui_extras::{Column, TableBuilder};
use kandrium_core::{ArchivedCard, Board};

use crate::{icons, theme};

/// Runtime UI state of the archive view (not persisted).
pub struct ArchiveState {
    pub search: String,
    /// "all" or a board id.
    pub board_filter: String,
    pub sort_newest: bool,
}

impl Default for ArchiveState {
    fn default() -> Self {
        Self {
            search: String::new(),
            board_filter: "all".to_string(),
            sort_newest: false,
        }
    }
}

/// What the user did this frame.
#[derive(Default)]
pub struct ArchiveActions {
    pub restore: Option<String>,
    /// Permanent delete requested (app shows the confirm dialog first).
    pub request_delete: Option<String>,
}

const HEADER_H: f32 = 30.0;
const ROW_H: f32 = 34.0;

/// Render the archive view.
pub fn show(
    ui: &mut Ui,
    cards: &[ArchivedCard],
    boards: &[Board],
    state: &mut ArchiveState,
) -> ArchiveActions {
    let mut out = ArchiveActions::default();
    let t = theme::Tokens::get(ui.ctx());

    ui.add_space(8.0);

    // Pre-compute the result count for the toolbar badge. This runs before
    // the search box renders, so the count can lag the list by one frame
    // while typing — imperceptible, and keeps the table itself fresh.
    let result_count = cards.iter().filter(|c| matches_filter(c, state)).count();

    ui.horizontal(|ui| {
        theme::search_field(ui, &mut state.search, "Search archived cards...", 260.0);
        board_filter_combo(ui, boards, &mut state.board_filter);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let n = result_count;
            ui.label(
                RichText::new(format!("{n} card{}", if n == 1 { "" } else { "s" }))
                    .text_style(theme::small_strong())
                    .color(t.text_faint),
            );
        });
    });

    let mut filtered: Vec<&ArchivedCard> =
        cards.iter().filter(|c| matches_filter(c, state)).collect();
    filtered.sort_by(|a, b| {
        let cmp = a.archived_at.cmp(&b.archived_at);
        if state.sort_newest {
            cmp.reverse()
        } else {
            cmp
        }
    });

    if filtered.is_empty() {
        ui.add_space(24.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("No archived cards found.").color(t.text_muted));
        });
        return out;
    }

    // Table rows pack edge-to-edge; no vertical gaps between them.
    let spacing = ui.spacing().item_spacing;
    ui.spacing_mut().item_spacing.y = 0.0;
    // TableBuilder builds its own ScrollArea (no content_margin hook), so
    // locally reserve the 7px bar lane to keep rows clear of the bar.
    ui.spacing_mut().scroll.floating_allocated_width = 7.0;

    let mut header_rect = Rect::NOTHING;
    TableBuilder::new(ui)
        .id_salt("archived-table")
        .striped(true)
        // Click sense enables the built-in per-row hover wash.
        .sense(Sense::click())
        .cell_layout(Layout::left_to_right(Align::Center))
        .auto_shrink(egui::Vec2b::new(false, false))
        .column(Column::remainder().at_least(180.0).clip(true)) // Card
        .column(Column::exact(90.0)) // Labels
        .column(Column::exact(130.0).clip(true)) // Board
        .column(Column::exact(120.0).clip(true)) // Column
        .column(Column::exact(170.0)) // Archived
        .column(Column::exact(100.0)) // Due
        .column(Column::exact(76.0)) // Actions
        .header(HEADER_H, |mut header| {
            header.col(|ui| {
                ui.add_space(8.0);
                header_cell(ui, "Card");
            });
            header.col(|ui| header_cell(ui, "Labels"));
            header.col(|ui| header_cell(ui, "Board"));
            header.col(|ui| header_cell(ui, "Column"));
            let (_rect, resp) = header.col(|ui| {
                header_cell(ui, "Archived");
                let (tri, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                sort_triangle(ui, tri, state.sort_newest, t.text_faint);
            });
            if resp
                .on_hover_text(if state.sort_newest {
                    "Newest first — click to reverse"
                } else {
                    "Oldest first — click to reverse"
                })
                .clicked()
            {
                state.sort_newest = !state.sort_newest;
            }
            header.col(|ui| header_cell(ui, "Due"));
            header.col(|_ui| {});
            header_rect = header.response().rect;
        })
        .body(|body| {
            body.rows(ROW_H, filtered.len(), |mut row| {
                let card = filtered[row.index()];

                // Card title (truncates; tooltip shows the full title).
                row.col(|ui| {
                    ui.add_space(8.0);
                    let font = theme::font_id(ui, TextStyle::Button);
                    let full_w = ui
                        .painter()
                        .layout_no_wrap(card.title.clone(), font, t.text)
                        .size()
                        .x;
                    let avail = ui.available_width();
                    let resp = ui.label(RichText::new(&card.title).text_style(TextStyle::Button));
                    if full_w > avail {
                        resp.on_hover_text(&card.title);
                    }
                });

                // Label dots.
                let (_rect, resp) = row.col(|ui| label_dots(ui, card));
                if !card.labels.is_empty() {
                    resp.on_hover_text(label_names(card));
                }

                // Board / Column.
                row.col(|ui| muted_cell(ui, &card.board_title, t.text_muted));
                row.col(|ui| muted_cell(ui, &card.column_title, t.text_muted));

                // Archived / Due dates.
                row.col(|ui| muted_cell(ui, &format_archived_at(&card.archived_at), t.text_muted));
                row.col(|ui| {
                    if card.due_date.is_empty() {
                        muted_cell(ui, "\u{2014}", t.text_faint);
                    } else {
                        muted_cell(ui, &card.due_date, t.text_muted);
                    }
                });

                // Actions.
                row.col(|ui| {
                    if icons::button(ui, "rotate-ccw", 24.0, false, "Restore").clicked() {
                        out.restore = Some(card.id.clone());
                    }
                    if icons::button_tinted(
                        ui,
                        "trash-2",
                        24.0,
                        ui.visuals().error_fg_color,
                        "Delete",
                    )
                    .clicked()
                    {
                        out.request_delete = Some(card.id.clone());
                    }
                });
            });
        });

    ui.spacing_mut().item_spacing = spacing;
    // Hairline under the sticky header, spanning the full table width.
    ui.painter().hline(
        header_rect.x_range(),
        header_rect.bottom(),
        Stroke::new(1.0, t.divider),
    );

    out
}

/// Search + board-filter predicate shared by the pre-count and the table list.
fn matches_filter(card: &ArchivedCard, state: &ArchiveState) -> bool {
    if !state.search.trim().is_empty() {
        let q = state.search.trim().to_lowercase();
        if !card.title.to_lowercase().contains(&q) {
            return false;
        }
    }
    state.board_filter == "all" || card.board_id == state.board_filter
}

/// Board-filter combo box styled to match [`theme::search_field`]: same
/// frame, same inner padding, and therefore the same height.
fn board_filter_combo(ui: &mut Ui, boards: &[Board], filter: &mut String) {
    let t = theme::Tokens::get(ui.ctx());

    let frame = egui::Frame::new()
        .fill(t.field)
        .stroke(Stroke::new(1.0, t.border))
        .corner_radius(egui::CornerRadius::same(t.radius))
        .inner_margin(egui::Margin::symmetric(8, 5));

    // The ComboBox draws its own button frame; neutralize it so only our
    // wrapper frame (matching the search field) is visible.
    let old_visuals = ui.visuals().clone();
    let old_padding = ui.spacing().button_padding;
    {
        let mut v = old_visuals.clone();
        for w in [
            &mut v.widgets.noninteractive,
            &mut v.widgets.inactive,
            &mut v.widgets.hovered,
            &mut v.widgets.active,
            &mut v.widgets.open,
        ] {
            w.bg_fill = Color32::TRANSPARENT;
            w.weak_bg_fill = Color32::TRANSPARENT;
            w.bg_stroke = Stroke::NONE;
            w.expansion = 0.0;
        }
        ui.visuals_mut().clone_from(&v);
        // Let the wrapper frame own all padding so the combo's height and
        // text inset match the search field exactly.
        ui.spacing_mut().button_padding = egui::Vec2::ZERO;
    }

    frame.show(ui, |ui| {
        egui::ComboBox::from_id_salt("archive-board-filter")
            .selected_text(selected_board_label(boards, filter))
            .width(160.0)
            .show_ui(ui, |ui| {
                ui.selectable_value(filter, "all".to_string(), "All Boards");
                for b in boards {
                    ui.selectable_value(filter, b.id.clone(), &b.title);
                }
            });
    });

    ui.visuals_mut().clone_from(&old_visuals);
    ui.spacing_mut().button_padding = old_padding;
}

fn header_cell(ui: &mut Ui, text: &str) {
    let t = theme::Tokens::get(ui.ctx());
    ui.label(
        RichText::new(text.to_uppercase())
            .text_style(theme::small_strong())
            .color(t.text_faint)
            .extra_letter_spacing(0.6),
    );
}

/// Muted body-text cell used for board/column/date columns.
fn muted_cell(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(RichText::new(text).text_style(TextStyle::Body).color(color));
}

/// Up to four 14x6 label dots (same shape as the board card chips), with a
/// "+n" suffix when the card has more.
fn label_dots(ui: &mut Ui, card: &ArchivedCard) {
    const MAX_DOTS: usize = 4;
    for label in card.labels.iter().take(MAX_DOTS) {
        if let Some(color) = theme::label_color(*label) {
            let (rect, _) = ui.allocate_exact_size(vec2(14.0, 6.0), Sense::hover());
            ui.painter().rect_filled(rect, 3.0, color);
        }
    }
    let extra = card.labels.len().saturating_sub(MAX_DOTS);
    if extra > 0 {
        ui.label(
            RichText::new(format!("+{extra}"))
                .text_style(TextStyle::Small)
                .color(theme::Tokens::get(ui.ctx()).text_faint),
        );
    }
}

/// Comma-separated label names for the dots tooltip.
fn label_names(card: &ArchivedCard) -> String {
    card.labels
        .iter()
        .filter_map(|i| theme::LABEL_COLORS.get(*i as usize))
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Small filled triangle indicating sort direction next to the ARCHIVED
/// header: points down for newest-first (descending), up for oldest-first.
fn sort_triangle(ui: &Ui, rect: Rect, newest_first: bool, color: Color32) {
    let (hw, hh) = (4.0, 2.5);
    let c = rect.center();
    let pts = if newest_first {
        vec![
            pos2(c.x - hw, c.y - hh),
            pos2(c.x + hw, c.y - hh),
            pos2(c.x, c.y + hh),
        ]
    } else {
        vec![
            pos2(c.x - hw, c.y + hh),
            pos2(c.x + hw, c.y + hh),
            pos2(c.x, c.y - hh),
        ]
    };
    ui.painter()
        .add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

fn selected_board_label(boards: &[Board], filter: &str) -> String {
    if filter == "all" {
        "All Boards".to_string()
    } else {
        boards
            .iter()
            .find(|b| b.id == filter)
            .map(|b| b.title.clone())
            .unwrap_or_else(|| filter.to_string())
    }
}

/// SQLite `datetime('now')` strings are already "YYYY-MM-DD HH:MM:SS"; show
/// them as-is (missing timestamps become an em dash).
fn format_archived_at(s: &str) -> String {
    if s.is_empty() {
        "\u{2014}".to_string()
    } else {
        s.to_string()
    }
}
