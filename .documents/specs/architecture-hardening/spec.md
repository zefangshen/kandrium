# Architecture hardening — implementation spec

**Source of truth:** [`architecture-review.md`](../../2026-11-14/architecture-review.md) (R1–R11,
plus the "leave alone" list L1–L11 and §7).
**Order:** subtract (Stage A), enforce boundaries (Stage B), de-duplicate (Stage C).
**Constraint:** one commit per stage; `cargo fmt --all`, `cargo clippy --workspace --all-targets`
and `cargo test --workspace` clean at every commit, so the tree is never half-refactored.
**Out of scope:** the wiki plan, and moving `pdf-render` under `core`.

The "leave alone" list is binding: `vim.rs`, `views/projects.rs`, `tests/shell.rs`,
`get_board_impl(reconcile)`, `Store`'s two handles, `KandriumError(String)`,
`Column.mirrored`, the 13-tool MCP surface, the three file trees, pinned gpui,
`AppShell` field regrouping, and id newtypes are not touched except where a finding
below explicitly says so.

---

## Stage A — subtract (commit 1)

### A1. Delete the four migration probes (R1)

Delete these binary-only modules (none is referenced from `lib.rs` or `tests/`):

- `crates/app/src/board.rs`
- `crates/app/src/drag_probe.rs`
- `crates/app/src/input_probe.rs`
- `crates/app/src/pdf_probe.rs`
- `crates/app/src/harness_test.rs` (only referenced by `main.rs` and `README.md`;
  nothing under `tests/` uses it)

Rewrite `crates/app/src/main.rs` so the product path is the only path:

```rust
//! Kandrium desktop entry point. Starts the `kandrium-app` shell.

fn main() {
    let _ = kandrium_core::logging::init("kandrium-app");
    kandrium_core::logging::install_panic_hook("kandrium-app");
    if let Some(arg) = std::env::args().nth(1) {
        eprintln!("usage: kandrium-app (got {arg:?})");
        std::process::exit(2);
    }
    kandrium_app::run();
}
```

`crates/app/README.md`: delete "The green path (Probes A–D)" (lines 58–75) and the
`harness_test` bullet in "Tests". Keep the `gpui-pre` dev-dependency — integration
tests import `gpui::TestAppContext`.

### A2. Stale-documentation sweep (R9)

Fix present-tense claims that are no longer true (historical "ported from egui"
comments stay):

| file | change |
|---|---|
| `crates/core/src/lib.rs:3` | drop "`kandrium-app` (egui)"; name the gpui app and the MCP server |
| `crates/pdf-render/src/lib.rs:1` | "shared by both front-ends" → the desktop app's Documents section |
| `crates/pdf-render/src/lib.rs:8` | drop "egui repaints"; the `wake` callback exists so gpui repaints while work is outstanding |
| `crates/app/src/vim.rs:29-33` | replace "the gpui shell has no settings store yet, so vim is always on for the description" with: the on/off preference is the shell's global `vim_mode` setting, passed in through `set_enabled` |
| `crates/app/src/lib.rs:6` | "bootstrapped exactly like the egui app and the MCP server" → like the MCP server |
| `crates/app/src/views/notes.rs:5` and `views/documents.rs:6` | "the same ones the egui app uses" → the same ones the MCP server / core uses |
| `crates/app/src/app.rs:6427` | "matching the sidecar timestamps the egui app writes" → matching the sidecar timestamp format |
| `AGENTS.md:38` | migration range stops at `0010`; end it at `0011_project_paths.sql`, and add v11 to the per-step list (project folder references) |
| `plan.md` | prepend a blockquote banner: historical snapshot as of 2026-11-05, not current; current plans live in `.documents/specs/` and rules in `AGENTS.md` |

### A3. Hygiene: one path for services, atomic new files (R11)

- `crates/core/src/lib.rs`: `pub mod services` → `mod services`. Every in-crate user
  (`seed.rs` tests, `import.rs` tests, `projects.rs`, `services/*`) already imports
  through `crate::services::…`, so a crate-private module is sufficient. Keep the flat
  re-exports.
- Replace new-file `fs::write(path, "")` with `fsutil::write_atomic(path, "")` at:
  - `crates/core/src/notes.rs:68` (`create_note`)
  - `crates/core/src/documents.rs:158` (`create_markdown`)
  - `crates/core/src/projects.rs:912` (`create_doc_file`) — **the review counted two
    sites; this third identical site is also covered, because the stated rule is
    "new-file creation is the same helper".**

### A4. MCP reads stop writing (R6)

- `crates/mcp/src/main.rs:188`: `core::get_board` → `core::get_board_readonly`.
- Add a short comment above `get_all_board` stating it has no read-only twin and
  allocates order rows under an `IMMEDIATE` transaction by design (do not change it).

**Stage A verification:** the three commands must pass. No test changes are expected.

---

## Stage B — enforce the boundaries (commit 2)

### B1. One boot path (R3)

In `crates/core/src/db/connection.rs`:

```rust
/// Integrity check + snapshot + schema. Every boot path goes through this.
pub fn prepare(db: &mut Connection, db_path: &Path) -> Result<()> {
    super::backup::check_integrity(db, db_path)?;
    let pending = super::migrations::pending_migrations(db)?;
    super::backup::snapshot_if_due(db, db_path, pending)?;
    super::migrations::run_schema(db)
}

pub fn bootstrap(db: &mut Connection, db_path: &Path) -> Result<()> {
    prepare(db, db_path)?;
    crate::seed::seed(db)
}
```

- `crates/core/src/import.rs`: replace `run_schema(&mut new)?` (line 64) with
  `prepare(&mut new, new_path)?`, so the importer gains the integrity check and one
  harmless empty-DB snapshot. The `has_boards` guard stays after `prepare`.
- `crates/core/src/db/migrations.rs:109`: `pub fn run_schema` → `pub(crate) fn run_schema`.
- `crates/core/src/db/mod.rs:21`: stop re-exporting `run_schema`; export `prepare`
  and `bootstrap` from `connection`.
- `crates/core/src/lib.rs:34-37`: drop `run_schema` from the `db` re-export, add
  `prepare`.
- In-crate test callers (`clocks.rs`, `seed.rs`, `backup.rs`, `migrations.rs`) keep
  working through `pub(crate)` / `crate::db::migrations::run_schema`.

### B2. Schema version is a checked invariant (R8)

Add to `crates/core/src/db/migrations.rs` tests:

- `schema_version_matches_the_migration_set`: `assert_eq!(migrations().len() as i64, SCHEMA_VERSION)`.
- `every_sql_file_is_registered`: read `env!("CARGO_MANIFEST_DIR")/../../migrations`,
  collect `*.sql` names in sorted order, and assert the set matches the embedded
  `MIGRATION_SQL` / migration list names.

### B3. CI and lints (R10)

- `.github/workflows/ci.yml`: one job running
  `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace`.
- `crates/mcp/Cargo.toml`: add

  ```toml
  [lints.clippy]
  print_stdout = "deny"
  ```

  (stderr stays allowed: logging mirrors to stderr).
- Do not add `unsafe_code = "forbid"` at workspace level; it requires `[lints]` in
  every crate and there is no `unsafe` today. Note this in the stage report.

### B4. Pin the MCP payload (R7)

Add one test in `crates/mcp` that:

1. builds a seeded temp database through `kandrium_core` (tempfile),
2. loads a board and a card, serialises both through the same `serde` path the tools
   use,
3. asserts the exact, sorted JSON key set for a `Board`, a `Column` (including
   `cards` and `mirrored`) and a `Card`.

Use a sorted `Vec<&str>` comparison so the failure diff is readable; no snapshot files.

**Stage B verification:** the three commands must pass. `cargo clippy` must be clean
with `-D warnings`, including the new `print_stdout` lint.

---

## Stage C — de-duplicate (commit 3)

### C1. One `Draft` type for the six debounced-save families (R2)

Add `crates/app/src/draft.rs`:

```rust
pub struct Draft {
    saved: String,   // last text confirmed on disk
    seen: String,    // last text observed from the editor
    dirty: bool,
    generation: u64,
}

impl Draft {
    pub fn new(saved: String) -> Self;
    /// `Some(gen)` only for a real change, `None` for unchanged text.
    pub fn observe(&mut self, value: String) -> Option<u64>;
    pub fn is_current(&self, generation: u64) -> bool;
    pub fn mark_saved(&mut self, value: String);
    pub fn dirty(&self) -> bool;
}
```

Add one `AppShell::debounce(duration, move |this, cx| …)` helper that owns the
stale-generation check once. Embed `Draft` in `NoteTab`, `DocTab`, `ProjectDocTab`,
`TitleEdit`, `DetailEditor`; replace the `ann_*` annotation fields with a `Draft` too.
Keep each family's existing save function and outer behaviour; keep the existing
autosave tests (`markdown_autosave_debounces_and_writes_once` et al.) passing
unchanged. Delete the three duplicate `AUTOSAVE` constants in favour of one.

**Risk:** this is the largest stage. If the shared helper cannot cover one family
without changing behaviour, stop, report the specific obstacle, and land C1 partially
only if the remaining families stay behaviour-identical.

### C2. One owner for identifier uniqueness (R5)

- `crates/core/src/projects.rs`: add
  `pub fn identifier_owner(db: &Connection, identifier: &str, exclude_id: Option<&str>) -> Result<Option<String>>`
  returning the owning project's name, using the existing `COLLATE NOCASE` query.
- `set_identifier` uses it to build its error message.
- Delete `AppShell::identifier_owner` (`crates/app/src/app.rs:2666-2682`) and its
  `flat_projects` call; the title-edit commit calls `core::projects::identifier_owner`
  on the write path instead of checking the cached tree.
- Export the function from `crates/core/src/lib.rs`.
- Add a core test: two projects, duplicate identifier rejected case-insensitively,
  `exclude_id` permits renaming a project to its own identifier.

### C3. Watcher moves into `core` (R4)

- Move `crates/app/src/watcher.rs` to `crates/core/src/db/watcher.rs` unchanged
  (std + rusqlite only; keep its two unit tests).
- `crates/core/src/db/mod.rs`: `pub mod watcher;` and re-export
  `Watcher` (with `spawn`, `drain`, `stop`).
- Delete `crates/app/src/watcher.rs` and `pub mod watcher` from
  `crates/app/src/lib.rs`; update every `crate::watcher` import in the app to
  `kandrium_core::db::watcher`.
- Update `AGENTS.md:27` and `crates/app/README.md` (tests + layout rows) to the new path.

**Stage C verification:** the three commands must pass.

---

## Commit discipline

Each stage is one commit, with the committer override:

```sh
GIT_COMMITTER_NAME=zefangshen GIT_COMMITTER_EMAIL=zefang.sh@gmail.com git commit -m "…"
```

No `Co-Authored-By`, no AI attribution, no push, no branches.

## Reporting

After each stage: what landed, the exact commit message, what contradicted the review,
and anything deferred because it was larger than the review assumed.
