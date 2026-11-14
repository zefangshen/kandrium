# Architecture hardening — tasks

Each task is one stage's smallest verifiable unit. A stage is committed only when
`cargo fmt --all`, `cargo clippy --workspace --all-targets` and
`cargo test --workspace` are clean.

## Stage A — subtract

- [x] A1 Delete `crates/app/src/{board,drag_probe,input_probe,pdf_probe,harness_test}.rs`.
- [x] A1 Rewrite `crates/app/src/main.rs` to the product path only, keeping an
      unknown-argument error.
- [x] A1 Remove the probe section and the `harness_test` bullet from
      `crates/app/README.md`.
- [x] A2 Fix the stale present-tense docs listed in `spec.md` §A2
      (`core/lib.rs`, `pdf-render/lib.rs`, `vim.rs`, `app/src/lib.rs`,
      `views/notes.rs`, `views/documents.rs`, `app.rs`, `AGENTS.md`, `plan.md`).
- [x] A3 Make `pub mod services` private in `crates/core/src/lib.rs`.
- [x] A3 Use `fsutil::write_atomic` for the three empty-file creation sites
      (`notes.rs:68`, `documents.rs:158`, `projects.rs:912`).
- [x] A4 Switch the MCP `get_board` tool to `core::get_board_readonly`; comment
      `get_all_board` as intentionally writing.
- [x] A Verify: `cargo fmt --all`, `cargo clippy --workspace --all-targets`,
      `cargo test --workspace`.
- [x] A Commit with the committer override and report.

## Stage B — enforce the boundaries

- [x] B1 Add `db::prepare`; make `bootstrap` call it, then seed.
- [x] B1 Point `import.rs` at `prepare`.
- [x] B1 Make `run_schema` `pub(crate)`; drop it from the `db` and `lib` re-exports;
      add `prepare` to both.
- [x] B2 Add the schema-version count test and the migration-file coverage test.
- [x] B3 Add `.github/workflows/ci.yml` (fmt check, clippy `-D warnings`, tests).
- [x] B3 Add `[lints.clippy] print_stdout = "deny"` to `crates/mcp/Cargo.toml`.
- [x] B4 Add the MCP payload key-set test.
- [x] B Verify and commit; report `unsafe_code = "forbid"` as deliberately omitted.

## Stage C — de-duplicate

- [ ] C1 Add `crates/app/src/draft.rs` and `AppShell::debounce`.
- [ ] C1 Embed `Draft` in `NoteTab`, `DocTab`, `ProjectDocTab`, `TitleEdit`,
      `DetailEditor`, and the annotation state; collapse the duplicate timers and
      `AUTOSAVE` constants. Stop and report if one family cannot keep behaviour.
- [ ] C2 Add `projects::identifier_owner`, use it in `set_identifier`, delete
      `AppShell::identifier_owner`, add the core test.
- [ ] C3 Move `watcher.rs` into `crates/core/src/db/`, update exports/imports and
      the two docs that name its path.
- [ ] C Verify, commit, and give the final report.
