//! Drag-and-drop engine for the Kanban board.
//!
//! egui gives us per-widget drag responses; the ordering logic lives here.
//! While rendering, each column/bucket body records its rect and the rects of
//! its cards (`DropZone`). On drag release, the pointer position is hit-tested
//! against those zones to compute the insertion point.

use egui::{Id, Rect};

use kandrium_core::CardStatus;

/// Owned key for a drop container (String ids come from the DB).
#[derive(Clone, PartialEq, Debug)]
pub enum ColKey {
    Column(String),
    Bucket(CardStatus),
}

/// One drop container: its on-screen rect plus ordered card slot rects.
#[derive(Clone, Debug)]
pub struct DropZone {
    pub key: ColKey,
    pub rect: Rect,
    /// Ordered card rects inside the container (excluding the dragged card).
    pub card_rects: Vec<(String, Rect)>,
}

impl DropZone {
    /// Compute the insertion index for a pointer position using the midpoint
    /// rule: the index is the number of card centers above the pointer.
    ///
    /// Boundaries therefore sit halfway between adjacent card centers, which
    /// covers the inter-card margins and the padding above the first card —
    /// regions the old `contains`-based rule treated as "end of list",
    /// making it nearly impossible to insert between two cards or before the
    /// topmost one.
    ///
    /// The rule is also what keeps the insertion gap stable: opening the gap
    /// only ever pushes cards *away* from the pointer, so a resting pointer
    /// resolves to the same index frame after frame. The old rule flipped to
    /// "end of list" as soon as the shifted card slid out from under the
    /// pointer, which bounced the gap (and the cards around it) up and down.
    pub fn insertion_index(&self, pos: egui::Pos2) -> usize {
        self.card_rects
            .iter()
            .filter(|(_, rect)| rect.center().y < pos.y)
            .count()
    }

    /// The card id the drop should land before (None = end of container).
    pub fn before_card_at(&self, index: usize) -> Option<&str> {
        self.card_rects.get(index).map(|(id, _)| id.as_str())
    }

    pub fn contains(&self, pos: egui::Pos2) -> bool {
        self.rect.contains(pos)
    }
}

/// Mutable drag state living in the app.
#[derive(Clone, Debug)]
pub struct Drag {
    pub card_id: String,
}

/// Find which zone the pointer is over, plus the insertion point.
pub fn hit_test(zones: &[DropZone], pos: egui::Pos2) -> Option<(usize, usize)> {
    zones
        .iter()
        .position(|z| z.contains(pos))
        .map(|zi| (zi, zones[zi].insertion_index(pos)))
}

/// Stable id for a card widget.
pub fn card_widget_id(card_id: &str) -> Id {
    Id::new(("kandrium-card", card_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    /// Zone with cards at the given (top, bottom) y-ranges.
    fn zone(card_ys: &[(f32, f32)]) -> DropZone {
        DropZone {
            key: ColKey::Column("c".into()),
            rect: Rect::from_min_max(pos2(0.0, 0.0), pos2(200.0, 1000.0)),
            card_rects: card_ys
                .iter()
                .enumerate()
                .map(|(i, (top, bottom))| {
                    (
                        format!("card-{i}"),
                        Rect::from_min_max(pos2(0.0, *top), pos2(200.0, *bottom)),
                    )
                })
                .collect(),
        }
    }

    // Reference stack: cards at 10..50 (center 30), 56..96 (center 76),
    // 102..142 (center 122) — 40px tall with 6px margins, like real chips.

    #[test]
    fn inserts_before_the_topmost_card() {
        let z = zone(&[(10.0, 50.0), (56.0, 96.0), (102.0, 142.0)]);
        assert_eq!(
            z.insertion_index(pos2(100.0, 0.0)),
            0,
            "padding above first card"
        );
        assert_eq!(
            z.insertion_index(pos2(100.0, 10.0)),
            0,
            "top edge of first card"
        );
        assert_eq!(
            z.insertion_index(pos2(100.0, 29.0)),
            0,
            "just above first center"
        );
        assert_eq!(
            z.before_card_at(z.insertion_index(pos2(100.0, 5.0))),
            Some("card-0")
        );
    }

    #[test]
    fn inserts_between_adjacent_cards_including_the_margin() {
        let z = zone(&[(10.0, 50.0), (56.0, 96.0), (102.0, 142.0)]);
        // The 6px margin between card 0 and card 1 used to fall through to
        // "end of list" — the main complaint. It must now resolve cleanly.
        assert_eq!(
            z.insertion_index(pos2(100.0, 53.0)),
            1,
            "inside inter-card margin"
        );
        assert_eq!(
            z.insertion_index(pos2(100.0, 31.0)),
            1,
            "just below card 0 center"
        );
        assert_eq!(
            z.insertion_index(pos2(100.0, 75.0)),
            1,
            "just above card 1 center"
        );
        assert_eq!(
            z.insertion_index(pos2(100.0, 77.0)),
            2,
            "just below card 1 center"
        );
    }

    #[test]
    fn inserts_at_the_end_below_the_last_card() {
        let z = zone(&[(10.0, 50.0), (56.0, 96.0)]);
        assert_eq!(
            z.insertion_index(pos2(100.0, 95.0)),
            2,
            "bottom edge of last card"
        );
        assert_eq!(
            z.insertion_index(pos2(100.0, 500.0)),
            2,
            "padding below last card"
        );
        assert_eq!(z.before_card_at(2), None, "end of container");
    }

    #[test]
    fn empty_zone_inserts_at_zero() {
        let z = zone(&[]);
        assert_eq!(z.insertion_index(pos2(100.0, 300.0)), 0);
    }

    #[test]
    fn stable_while_the_gap_shifts_cards_down() {
        // Reproduces the up-and-down flicker: a one-stride gap (46px) is open
        // at index 1, so cards 1..2 are pushed down to 102..142 / 148..188.
        // A pointer resting INSIDE the open gap must keep resolving to index
        // 1. The old `contains` rule found no card under the pointer and
        // returned `len`, moving the gap away and oscillating every frame.
        let gapped = zone(&[(10.0, 50.0), (102.0, 142.0), (148.0, 188.0)]);
        assert_eq!(
            gapped.insertion_index(pos2(100.0, 56.0)),
            1,
            "top lip of open gap"
        );
        assert_eq!(
            gapped.insertion_index(pos2(100.0, 79.0)),
            1,
            "middle of open gap"
        );
        assert_eq!(
            gapped.insertion_index(pos2(100.0, 101.0)),
            1,
            "bottom lip of open gap"
        );

        // And the follow-up frame: if the pointer then drifts below card 1's
        // (shifted) center, the gap moves down once and stays there — card 1
        // moves back up, away from the pointer, never oscillating.
        let moved = zone(&[(10.0, 50.0), (56.0, 96.0), (148.0, 188.0)]);
        assert_eq!(moved.insertion_index(pos2(100.0, 130.0)), 2);
    }
}
