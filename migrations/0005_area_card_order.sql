-- Kandrium schema v5 — unified display order for non-project ("area") boards.
--
-- A Tasks board shows both its own cards and the project cards filed to it
-- with "Show on Tasks boards" (`cards.area_board_id`). Native cards keep their
-- real column/`cards.position`; project cards keep their real project column.
-- This table holds the ONE order the board renders, so the two kinds of card
-- can be freely interleaved and dragged.
--
-- One row per card visible on the board (`status` = canonical bucket,
-- `position` = index inside that bucket). `services::board::get_board`
-- backfills and reconciles the rows lazily; `move_area_card` rewrites them on
-- drag/drop. Additive only: earlier schema files stay byte-identical.

CREATE TABLE IF NOT EXISTS area_card_order (
    board_id TEXT NOT NULL REFERENCES boards(id) ON DELETE CASCADE,
    card_id  TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    status   TEXT NOT NULL,
    position INTEGER NOT NULL,
    PRIMARY KEY (board_id, card_id)
);

CREATE INDEX IF NOT EXISTS idx_area_card_order_slot
    ON area_card_order(board_id, status, position);
