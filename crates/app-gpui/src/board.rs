//! A. Rendering: three columns of real cards, read from the live database
//! through `kandrium-core`, rendered with gpui + gpui-kit primitives.
//!
//! The live DB is never opened in place: `get_board` reconciles
//! `area_card_order`, so the spike copies the file (plus `-wal`/`-shm`) to a
//! temp directory and opens the copy. That keeps the probe read-only in
//! practice while still exercising the real service functions unchanged.

use gpui_kit::*;
use kandrium_core as core;

pub fn run() {
    let board = load();

    println!(
        "[A] loaded board {:?} with {} columns ({} cards)",
        board.title,
        board.columns.len(),
        board.columns.iter().map(|c| c.cards.len()).sum::<usize>()
    );
    application().run(move |cx| {
        gpui_kit::init(cx);
        let board = board.clone();
        open_window(WindowOptions::default(), cx, move |_window, cx| {
            cx.new(|_| BoardView {
                board: board.clone(),
                printed: false,
            })
        })
        .expect("open board window");
    });
}

fn load() -> core::BoardWithColumns {
    let src = core::resolve_db_path();
    let dir = std::env::temp_dir().join(format!("kandrium-spike-db-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp db dir");
    let dst = dir.join("kandrium.db");

    std::fs::copy(&src, &dst).unwrap_or_else(|e| panic!("copy {}: {e}", src.display()));
    for suffix in ["-wal", "-shm"] {
        let from = src.with_extension(format!("db{suffix}"));
        if from.exists() {
            let to = dst.with_extension(format!("db{suffix}"));
            std::fs::copy(&from, &to).ok();
        }
    }
    println!("[A] db copy: {}", dst.display());

    let conn = core::open_db(&dst).expect("open db copy");
    let boards = core::list_boards(&conn).expect("list_boards").boards;
    let target = boards
        .iter()
        .find(|b| b.title.eq_ignore_ascii_case("career"))
        .or_else(|| boards.first())
        .expect("no boards in the database")
        .id
        .clone();
    core::get_board(&conn, &target).expect("get_board")
}

struct BoardView {
    board: core::BoardWithColumns,
    printed: bool,
}

impl Render for BoardView {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        if !self.printed {
            self.printed = true;
            println!(
                "[A] first frame built: {} columns",
                self.board.columns.len()
            );
        }
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x1e1f22))
            .text_color(rgb(0xd9d9d9))
            .p_4()
            .gap_3()
            .child(div().text_xl().child(self.board.title.clone()))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .gap_3()
                    .overflow_hidden()
                    .children(self.board.columns.iter().map(|column| {
                        let cards = column.ordered_cards();
                        div()
                            .flex()
                            .flex_col()
                            .w(px(300.0))
                            .gap_2()
                            .p_2()
                            .rounded_md()
                            .bg(rgb(0x26272b))
                            .child(div().text_sm().text_color(rgb(0x9aa0a6)).child(format!(
                                "{} · {}",
                                column.title,
                                cards.len()
                            )))
                            .children(cards.into_iter().map(|card| {
                                div()
                                    .p_2()
                                    .rounded_md()
                                    .bg(rgb(0x2f3136))
                                    .child(card.title.clone())
                            }))
                    })),
            )
    }
}
