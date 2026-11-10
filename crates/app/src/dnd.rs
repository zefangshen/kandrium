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

/// A wrapping grid drop target: the grid bounds plus its laid-out slots in
/// row-major order (the dragged item included, so a row never disappears).
///
/// Unlike [`DropZone`] this resolves an index in both axes, so a pointer
/// chooses the row by its half-open vertical band and then the position
/// within that row by x.
#[derive(Clone, Debug)]
pub struct GridZone {
    pub rect: Rect,
    pub slots: Vec<(String, Rect)>,
}

impl GridZone {
    /// Row-major insertion index for a pointer inside the grid. Rows are
    /// derived from the slot tops, so the caller needs no column count.
    ///
    /// Pass the *laid-out* slots (one per grid position) rather than a
    /// reflowed preview: the bands only stay stable under the pointer if the
    /// geometry does not change while cards make room.
    pub fn insertion_index(&self, pos: egui::Pos2) -> usize {
        let Some((_, first)) = self.slots.first() else {
            return 0;
        };
        let mut row_starts = vec![0usize];
        let mut row_top = first.top();
        for (index, (_, rect)) in self.slots.iter().enumerate().skip(1) {
            if (rect.top() - row_top).abs() > 1.0 {
                row_starts.push(index);
                row_top = rect.top();
            }
        }

        // Rows are half-open vertical bands: the pointer belongs to the first
        // row whose bottom-to-next-top gap still lies below it. Anchoring the
        // boundary to the GAP (not the row's own centre) keeps the whole row
        // selectable — a pointer over the lower half of a card must not jump
        // to the next row. The last row is unbounded below.
        let mut row = row_starts.len() - 1;
        for candidate in 0..row_starts.len() {
            let end = row_starts
                .get(candidate + 1)
                .copied()
                .unwrap_or(self.slots.len());
            let bottom = self.slots[end - 1].1.bottom();
            let boundary = row_starts
                .get(candidate + 1)
                .map(|&next| (bottom + self.slots[next].1.top()) * 0.5)
                .unwrap_or(f32::INFINITY);
            if pos.y < boundary {
                row = candidate;
                break;
            }
        }

        let start = row_starts[row];
        let end = row_starts.get(row + 1).copied().unwrap_or(self.slots.len());
        let before = self.slots[start..end]
            .iter()
            .filter(|(_, rect)| rect.center().x < pos.x)
            .count();
        start + before
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
    fn grid_insertion_walks_rows_then_columns() {
        let slot = |x: f32, y: f32| Rect::from_min_size(pos2(x, y), egui::vec2(200.0, 156.0));
        let grid = GridZone {
            rect: Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(624.0, 324.0)),
            slots: vec![
                ("a".into(), slot(0.0, 0.0)),
                ("b".into(), slot(212.0, 0.0)),
                ("c".into(), slot(424.0, 0.0)),
                ("d".into(), slot(0.0, 168.0)),
                ("e".into(), slot(212.0, 168.0)),
            ],
        };
        // Row 0: before a, between a/b, after c.
        assert_eq!(grid.insertion_index(pos2(10.0, 10.0)), 0);
        assert_eq!(grid.insertion_index(pos2(210.0, 10.0)), 1);
        assert_eq!(grid.insertion_index(pos2(600.0, 10.0)), 3);
        // Row 1: before d, between d/e, after e.
        assert_eq!(grid.insertion_index(pos2(10.0, 300.0)), 3);
        assert_eq!(grid.insertion_index(pos2(210.0, 300.0)), 4);
        assert_eq!(grid.insertion_index(pos2(600.0, 300.0)), 5);
        // Above the first row center and below the last row's own center.
        assert_eq!(grid.insertion_index(pos2(210.0, 70.0)), 1);
        assert_eq!(grid.insertion_index(pos2(210.0, 260.0)), 4);
        // Outside the grid rect is rejected by `contains`, not the index.
        assert!(!grid.contains(pos2(10.0, -10.0)));
        assert!(grid.contains(pos2(10.0, 10.0)));
    }

    /// A row owns its whole card height, not just the half above its
    /// vertical centre. The previous rule switched rows at the centre of the
    /// row's own extent, so dipping into the lower half of a first-row card
    /// already resolved to the second row — the seam that broke cross-row
    /// drags (the target index jumped by a whole row while the pointer was
    /// still over the same card).
    #[test]
    fn grid_row_bands_use_the_row_gap_not_the_row_center() {
        let slot = |x: f32, y: f32| Rect::from_min_size(pos2(x, y), egui::vec2(200.0, 156.0));
        let grid = GridZone {
            rect: Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(624.0, 324.0)),
            slots: vec![
                ("a".into(), slot(0.0, 0.0)),
                ("b".into(), slot(212.0, 0.0)),
                ("c".into(), slot(424.0, 0.0)),
                ("d".into(), slot(0.0, 168.0)),
                ("e".into(), slot(212.0, 168.0)),
            ],
        };

        // Lower half of the first row stays in the first row.
        assert_eq!(grid.insertion_index(pos2(210.0, 100.0)), 1);
        assert_eq!(grid.insertion_index(pos2(210.0, 155.0)), 1);
        // The inter-row gap keeps resolving to the row above until its
        // midpoint, then flips to the next row.
        assert_eq!(grid.insertion_index(pos2(210.0, 160.0)), 1);
        assert_eq!(grid.insertion_index(pos2(210.0, 165.0)), 4);
        // Second row keeps its own full height, including its bottom edge.
        assert_eq!(grid.insertion_index(pos2(210.0, 300.0)), 4);
        assert_eq!(grid.insertion_index(pos2(10.0, 323.0)), 3);
    }

    #[test]
    fn single_row_grid_counts_only_x() {
        let slot = |x: f32| Rect::from_min_size(pos2(x, 0.0), egui::vec2(200.0, 156.0));
        let grid = GridZone {
            rect: Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(412.0, 156.0)),
            slots: vec![("a".into(), slot(0.0)), ("b".into(), slot(212.0))],
        };
        assert_eq!(grid.insertion_index(pos2(10.0, 10.0)), 0);
        assert_eq!(grid.insertion_index(pos2(300.0, 10.0)), 1);
        assert_eq!(grid.insertion_index(pos2(900.0, 10.0)), 2);
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
