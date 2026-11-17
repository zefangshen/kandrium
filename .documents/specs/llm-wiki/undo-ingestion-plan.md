# Undoable ingestion: file and row history

Status: implemented. Stages 0–8 landed on the `llm-wiki` branch; the plan is
kept as the design record.

This plan adds a run-scoped history store that makes an ingestion run undoable as
one unit. It covers both sides of the application:

- **Files**: bytes of every file a run changes, captured before mutation.
- **Rows**: per-run database snapshots and a derived table diff covering every
  app-owned row a run changes.

SQLite stays derived where it already is (the wiki index); file bytes and
app-owned rows both become recoverable. Verified code facts are cited. Anything
not verified is marked **unverified**.

Revision note: the file-history design from the first draft is unchanged. Section
3A adds project documents and full directory-delete recovery; §3B adds the row
side and the task-sized run boundary (one database-enforced open run with a
heartbeat); §10 separates the settled decisions from the remaining open
questions and records exactly what earlier drafts had to change.

---

## 1. Scope

### Captured roots and kinds (files)

| Root | Default | Captured kinds | Notes |
|---|---|---|---|
| Wiki pages | `KANDRIUM_WIKI` | `.md`, `.markdown` | Includes `imported/`, compiled folders, and root pages. `index.md` is excluded (generated). |
| Personal notes | `KANDRIUM_NOTES` | `.md`, `.markdown` | The Notes half of Knowledge. Directory deletes capture every file under the directory. |
| Documents | `KANDRIUM_DOCS` | `.md`, `.markdown`, `.pdf`, `*.pdf.kandrium.json` | Markdown documents are editable in the Documents half; PDFs are captured when created/deleted, sidecars whenever they change. Import supports PDF/Markdown only ([documents.rs L190](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/documents.rs#L190)). |
| PDF annotation sidecars | beside the PDF under `KANDRIUM_DOCS` | `*.pdf.kandrium.json` | Written by `documents::save_annotations` ([L268](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/documents.rs#L268)); deleted with the PDF ([`delete_path` L322](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/documents.rs#L322)). |
| Project documents | `KANDRIUM_PROJECTS` | every file under `<projects_root>/<project_id>/` | Captured on app writes, imports, and project/folder deletion. See §3A for path keying, folder capture, and full directory restore. |

### Captured rows (database)

| Entity | Tables | Notes |
|---|---|---|
| Tasks | `boards`, `columns`, `cards`, `all_card_order`, `area_card_order` | Cards, order, archive flags, board structure. Columns have app-only mutations; they are still rows and are captured. |
| Projects | `project_folders`, `projects`, `project_paths` | Folder tree, identifier, ranges, archived state, and folder references. |
| Clocks | `clocks`, `clock_people`, `clock_travels` | Cities, people, travels. `clocks_use_24h` is a settings key, not a row. |
| Derived wiki index | `wiki_pages`, `wiki_page_sources`, `wiki_links`, `wiki_runs`, `wiki_fts` | **Not captured.** Rebuilt from files. See §3B. |

### Explicitly out of scope (v1)

- Files under a project folder that no captured run touched (created or edited
  outside the app). A run that deletes the project or folder still captures them
  before removal.
- Project folders referenced through `project_paths`; only the reference row is
  undoable, never the external directory.
- Images and other binaries inside the wiki, notes and documents roots, except
  when a recursive delete captures them. Project folders are different: every
  file there is captured on write, upload or deletion (see §3A).
- The SQLite database as a whole: schema, migrations, `VACUUM`, WAL state.
- `index.md` at the wiki root; regenerated after restore.
- `gpui-settings.conf`, logs, lock files, and the history store itself.
- Git repositories or worktrees inside the roots. The store is filesystem- and
  row-level, not git-aware.

### Definitions

- **Run**: one unit of agent or user work that may touch many files and rows.
- **Pre-image**: the bytes of a path immediately before the run first mutates
  it, or the row values present in the start snapshot.
- **After-image**: for files, only its hash (conflict detection); for rows, the
  row hash recorded in the derived diff.
- **Row diff**: the per-table list of inserted, updated and deleted keys plus
  before/after hashes, derived from the start and end snapshots.

---

## 2. Data model

### On-disk layout

```
<data_dir>/kandrium/history/
  objects/<first-two-hex>/<full-content-hash>   # deduplicated file pre-images
  db/<run-id>-start.db                          # VACUUM INTO at run start
  db/<run-id>-rows.json                         # derived table diff
  runs/<run-id>.json                            # manifest, rewritten atomically
```

- `objects/` is content-addressed and shared across runs and roots. A hash
  already present is not written again. Algorithm: reuse `sources::hash_bytes`;
  the exact algorithm is **unverified** and unchanged by this plan.
- `db/<run-id>-start.db` is a full database snapshot taken with
  `VACUUM INTO` at run start. The end snapshot is temporary: it is diffed and
  deleted after the row diff is derived (see §3B).
- `db/<run-id>-rows.json` is the derived per-table diff: keys and row hashes
  only, plus counts. Before values live in the start snapshot.
- `runs/<run-id>.json` is rewritten on each capture through
  `fsutil::write_atomic`. The history store itself is never captured.
- Run id: `db::uid()` (24 lowercase hex) when the caller does not supply one.

### Manifest

```json
{
  "version": 1,
  "runId": "…",
  "kind": "explicit | implicit | app",
  "label": "ingest the attention papers",
  "closedBy": "explicit | exit | abandoned | action",
  "heartbeatAt": 1760000009,
  "operation": "compile | patch | delete | user-save | import",
  "agent": "codex",
  "startedAt": 1760000000,
  "finishedAt": 1760000012,
  "state": "open | committed | undone | files-restored | failed | partial",
  "files": [
    {
      "seq": 1,
      "root": "wiki | notes | documents | projects",
      "path": "concepts/attention.md",
      "op": "modify | create | delete | rename",
      "before": { "hash": "…", "mode": 420, "size": 1234 },
      "after":  { "hash": "…", "mode": 420, "size": 1301 },
      "capturedAt": 1760000004,
      "oversize": false,
      "restorable": true,
      "reason": null,
      "sidecar": false
    }
  ],
  "dirs": [
    {
      "seq": 2,
      "root": "projects",
      "path": "<project_id>/papers",
      "op": "mkdir | rmdir",
      "mode": 493,
      "empty": false,
      "capturedAt": 1760000004
    }
  ],
  "links": [
    {
      "seq": 3,
      "root": "projects",
      "path": "<project_id>/current",
      "target": "papers/2026",
      "capturedAt": 1760000004
    }
  ],
  "rows": {
    "snapshot": "db/<run-id>-start.db",
    "snapshotBytes": 274432,
    "userVersion": 12,
    "schema": { "boards": "…", "cards": "…" },
    "diff": "db/<run-id>-rows.json",
    "counts": { "inserted": 1, "updated": 5, "deleted": 1 },
    "contended": false,
    "restorable": true
  }
}
```

- `before` is `null` for `create`; `after` is `null` for `delete`.
- `mode` preserves Unix permission bits only. Ownership, ACLs and xattrs are not
  preserved in v1.
- `rename` is a `delete` of the old path plus a `create` of the new path.
- `reviewed` and all frontmatter live inside file bytes and are restored with
  them. Rows carry their own before values.
- `restorable: false` marks an entry the run cannot restore. `reason` says why:
  `oversize` (above the 256 MB cap), `unreadable`, `special` (FIFO, socket,
  device), or `budget` (the shared ceiling would be exceeded). Undo restores
  every restorable entry and lists the rest by path and reason; only those
  entries make the run `partial`. Policy exclusions are listed separately and do
  not count.
- `dirs` records every directory a recursive delete or folder create touches,
  with its mode and an `empty` flag. A file-only manifest cannot recreate empty
  directories or the tree shape, so directories are first-class entries.
- `links` records symlinks by target string. Symlinks are never followed during
  capture or restore; the link itself is recreated.
- `heartbeatAt` mirrors `run_undo.last_heartbeat_at`, which is authoritative.
  It advances on every run-scoped MCP call and on `heartbeat_wiki_run`.

### Database

A new additive migration `migrations/0013_run_history.sql`, registered in
`db::migrations` with `SCHEMA_VERSION` bumped, adds:

```sql
CREATE TABLE IF NOT EXISTS run_history (
    run_id      TEXT NOT NULL,
    seq         INTEGER NOT NULL,
    kind        TEXT NOT NULL DEFAULT 'file',
    root        TEXT NOT NULL,
    path        TEXT NOT NULL,
    op          TEXT NOT NULL,
    before_hash TEXT,
    after_hash  TEXT,
    mode        INTEGER,
    size        INTEGER,
    target      TEXT,
    empty       INTEGER NOT NULL DEFAULT 0,
    reason      TEXT,
    restorable  INTEGER NOT NULL DEFAULT 1,
    captured_at INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (run_id, seq)
);

CREATE INDEX IF NOT EXISTS idx_run_history_path ON run_history (path);
CREATE INDEX IF NOT EXISTS idx_run_history_run  ON run_history (run_id);

CREATE TABLE IF NOT EXISTS run_row_changes (
    run_id         TEXT PRIMARY KEY NOT NULL,
    label          TEXT NOT NULL DEFAULT '',
    kind           TEXT NOT NULL DEFAULT 'implicit',
    closed_by      TEXT NOT NULL DEFAULT '',
    snapshot       TEXT NOT NULL,
    snapshot_bytes INTEGER NOT NULL DEFAULT 0,
    user_version   INTEGER NOT NULL DEFAULT 0,
    schema_json    TEXT NOT NULL DEFAULT '{}',
    diff_path      TEXT NOT NULL DEFAULT '',
    inserted       INTEGER NOT NULL DEFAULT 0,
    updated        INTEGER NOT NULL DEFAULT 0,
    deleted        INTEGER NOT NULL DEFAULT 0,
    contended      INTEGER NOT NULL DEFAULT 0,
    restorable     INTEGER NOT NULL DEFAULT 1,
    captured_at    INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS run_undo (
    run_id            TEXT PRIMARY KEY NOT NULL,
    state             TEXT NOT NULL DEFAULT 'open',
    last_heartbeat_at INTEGER NOT NULL DEFAULT 0,
    undone_at         INTEGER,
    note              TEXT
);

-- At most one open run, enforced by SQLite rather than by convention.
CREATE UNIQUE INDEX IF NOT EXISTS idx_run_undo_one_open
    ON run_undo (state) WHERE state = 'open';
```

- `run_history.kind` is `file`, `dir` or `link`; `empty` is set only for
  directories. `reason` carries the unrestorable cause from the manifest.
- `run_undo.last_heartbeat_at` is the single staleness signal. It advances on
  `begin_wiki_run`, every run-scoped MCP call while the run is open, and
  `heartbeat_wiki_run`. A run with no heartbeat for 10 minutes is reclaimable;
  the 24-hour sweep uses the same field and does not keep a second
  stale-detection rule.
- The partial unique index makes a second open row impossible. `begin_wiki_run`
  can only succeed after the previous open row leaves `open` (explicitly closed,
  reclaimed, or marked abandoned).
- Implicit and app runs are inserted with `state = 'committed'` when they finish,
  so they never hold the open slot and never contend for it.
- No foreign key to `wiki_runs`: a run may be implicit (user actions) and the
  ledger row may be written after the changes. `run_id` is the join key.
- `wiki_runs` needs no schema change.
- The manifest is authoritative; both tables are queryable mirrors and can be
  rebuilt from manifests.
- One additive migration. No edit to `0012_wiki.sql`.

### Run linkage

Current write APIs carry no run id — `WritePageSpec` and `PatchPageSpec` have
`agent` and `instructions_version` but no `run_id`
([write.rs L37-L44, L58-L66](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/wiki/write.rs#L37-L66)).
Plan:

- Add `pub run_id: Option<String>` to `WritePageSpec`, `PatchPageSpec`, and
  `DeletePageSpec`; add optional `runId` to `RunRecord`.
- Add `run_id` to the history API, not to every core service function. Row
  capture is snapshot-based and run-scoped (§3B), so service signatures do not
  change.
- Explicit runs are opened by `begin_wiki_run` and closed by `end_wiki_run`;
  implicit runs are created per mutating MCP call when none is open; the app
  records one per mutating `Store` action, inserted `committed` at close. The MCP
  server keeps its own run id in memory but treats the `run_undo` open row as
  authoritative, so a second client sees the same open run and is refused.
- Results (`PageWrite`, `PagePatch`, MCP responses) report the `runId`.

---

## 3. File capture points

Every file mutation below calls the same guard before touching the filesystem:

```rust
let guard = history::capture(&ctx, root_kind, path)?; // pre-image + manifest entry
// ... mutate file ...
guard.commit(&ctx, new_bytes_or_none)?;              // after hash
```

| Function | Mutation | Capture |
|---|---|---|
| `wiki::write::write_page` ([L140](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/wiki/write.rs#L140)) | overwrite/create page | pre-image if present; after hash; skip `index.md` |
| `wiki::write::patch_page` ([L203](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/wiki/write.rs#L203)) | delegates to `write_page` | inherits; no separate entry |
| `wiki::write::delete_page` ([L244](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/wiki/write.rs#L244)) | removes file and rows | pre-image before `fs::remove_file`; after = none |
| `documents::import_file` ([L190](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/documents.rs#L190)) | creates a copy | `create` entry with after hash |
| `documents::delete_path` ([L322](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/documents.rs#L322)) | removes document, PDF sidecar, or a directory recursively | tree enumeration before removal: pre-images for files, `dirs` entries, link targets; sidecar pre-image |
| `documents::save_annotations` ([L268](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/documents.rs#L268)) | creates/overwrites sidecar | pre-image if present; no-op when unchanged |
| `documents::write_text` ([L124](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/documents.rs#L124)) | saves Markdown | pre-image if present |
| `documents::create_markdown` ([L145](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/documents.rs#L145)) | creates Markdown | `create` entry |
| `documents::rename_path` ([L306](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/documents.rs#L306)) | renames document | delete old + create new |
| `notes::write_note` ([L50](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/notes.rs#L50)) | saves note or wiki page | pre-image if present |
| `notes::rename_path` ([L96](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/notes.rs#L96)) | renames note | delete old + create new |
| `notes::delete_path` ([L153](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/notes.rs#L153)) | removes file or directory recursively (`fs::remove_dir_all`) | tree enumeration before removal: pre-images for files, `dirs` entries including empty ones, link targets |
| `projects::write_doc` ([L919](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L919)) | overwrites a project text document | pre-image if present; after hash |
| `projects::create_doc_file` ([L931](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L931)) | creates an empty project document | `create` entry |
| `projects::create_doc_folder` ([L948](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L948)) | creates an empty folder | `dirs` op `mkdir`; empty folders are visible in the project docs tree |
| `documents::import_file` with the projects root as `root` | uploads a PDF/Markdown into a project ([app.rs L3285](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/app/src/app.rs#L3285)) | `create` entry; the root parameter classifies it as `projects` |
| `projects::delete_project` ([L721](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L721)) | deletes rows, then `remove_dir_ignore_missing` | complete tree enumeration before the SQL transaction: `delete` per file, `rmdir` per directory (including empty), link targets, unrestorable reasons |
| `projects::delete_folder` ([L843](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L843)) | deletes descendant projects and their directories | the same complete tree enumeration for every affected project id before the transaction |

App saves already funnel through these: `AppShell::save_note` (app.rs L4660) →
`Store::write_note` → `notes::write_note`; `save_document_tab` Markdown branch
(L5621) → `Store::write_document` → `documents::write_text`;
`save_annotations_tab` (L5651) → `Store::save_annotations` →
`documents::save_annotations`.

Deliberate policy exclusions never make a run partial: `index.md`, temp files,
`.DS_Store`, the history root, and byte-identical writes. They are the only
files a directory delete does not restore, because they are disposable OS or
generated content. Entries that cannot be restored for any other reason
(oversize, unreadable, special files) are recorded with `restorable: false` and a
`reason`, and they are the only source of a `partial` label. A test enumerates
allowed `fsutil::write_atomic` call sites so a future mutation cannot bypass
capture.

---

## 3A. Project documents

### Path keying

Project folders are keyed by the immutable project id:
`project_dir(root, id) = <projects_root>/<id>` ([projects.rs L188](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L188)).
The manifest therefore records:

```json
{ "root": "projects", "path": "<project_id>/<rel_path_inside_project>" }
```

This key survives:

- **Rename**: `rename_project` updates `projects.name` only; the directory is
  untouched ([L356](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L356)).
- **Identifier change**: `set_identifier` updates the identifier column only
  ([L524](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L524)).
- **Archive/unarchive**: rows change; the comment on `archive_project` states the
  on-disk folder stays untouched ([L741](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L741)).
- **Folder moves**: only `project_folders`/`projects.folder_id` change.

Restore re-joins `path` against the *current* `KANDRIUM_PROJECTS` root. If the
projects root env var changed between the run and the undo, files land at the
new root; the report flags "restored under a different projects root".

If the project row no longer exists at undo time (deleted later, outside this
run), restore recreates `<projects_root>/<project_id>/` with no owning row; the
report marks it an orphan project folder so the user can adopt or remove it.

### Capture points

| Operation | Function | Capture |
|---|---|---|
| Save/overwrite | `projects::write_doc` ([L919](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L919)) | pre-image if present, after hash |
| Create empty doc | `projects::create_doc_file` ([L931](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L931)) | `create` |
| Create empty folder | `projects::create_doc_folder` ([L948](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L948)) | `dirs` op `mkdir`; empty folders are user-visible in the project docs tree |
| Upload PDF/Markdown | `documents::import_file` with the project folder as `root` ([L190](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/documents.rs#L190), called from `upload_project_documents` [app.rs L3285](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/app/src/app.rs#L3285)) | `create`; the caller tags the root as `projects` (the current `root` argument is the project folder, so classification compares it against the projects root). Only PDF/Markdown pass the extension filter ([L198-L202](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/documents.rs#L198-L202)). |
| Delete project | `projects::delete_project` ([L721](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L721)) | complete tree enumeration before the SQL transaction: `delete` per file, `rmdir` per directory (including empty), link targets, and `restorable: false` + reason for what cannot be captured |
| Delete folder tree | `projects::delete_folder` ([L843](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L843)) | the same complete enumeration for every affected project directory before the transaction |
| Delete/rename one doc | **does not exist today** | no capture point exists; see below |

There is no per-document delete or rename in the core or the app: the project
docs surface only creates, writes and scans files. `delete_project` and
`delete_folder` are the only deletion paths. If per-doc delete/rename is added
later, it must be a core function (`projects::delete_doc` / `rename_doc`) with
the same guard; a test should fail if a project-root `fs::remove_file` or
`fs::rename` appears without capture.

### Scan or per-operation

Per-operation, like the other roots. A scan at run start would miss files
created earlier in the run and would capture unrelated files. The one place a
scan is required is a recursive delete: every directory-shaped deletion goes
through one `history::capture_tree(root_kind, dir)` helper that enumerates the
whole tree immediately before removal, so every entry that exists at that moment
— including uploads added outside the app — is captured.

### Directory deletion: one rule

Every recursive delete uses one helper and one order, whichever root it is in:
`notes::delete_path` (notes folders), `documents::delete_path` (documents
folders), `projects::delete_project`, `projects::delete_folder`, and any future
wiki-folder or source-folder delete. `history::capture_tree` runs **before the
first entry is removed** and records:

- a pre-image for every regular file through the file-history store;
- a `dirs` entry for every directory, parent-first, with its mode and whether it
  was empty — a file manifest alone cannot recreate empty directories or the
  shape;
- a `links` entry for every symlink, by target. Symlinks are recorded, never
  followed.

Because capture completes before deletion starts, a failure or crash partway
through the tree delete still leaves a complete manifest; the run is retryable or
reported `failed`, never half-captured.

Restore order is fixed: recreate directories parent-first, write files and
links, then restore rows (§4). Rows reference paths, so the files must exist
before the row phase commits.

`partial` is the exception, not the normal case. A project or folder delete whose
entries are all capturable is a full undo. Every regular file in the deleted tree
is captured regardless of extension, including images and office files under
notes or documents. Only these entries are unrecoverable, and each is listed in
History with its path and reason:

- `oversize`: a regular file above the 256 MB cap.
- `unreadable`: a file that cannot be read at capture time (including one that
  vanished mid-walk).
- `special`: FIFOs, sockets, devices and other non-regular entries.
- Policy exclusions (`.DS_Store`, temp files, `index.md`) are not restored
  because they are disposable, and they do not make a run partial.

No regular file extension is excluded from a directory delete: every regular
file under the deleted tree is captured, whatever its type. The History entry
shows the restored counts and, only when the cases above occur, a "partial"
label with the skipped paths and their reasons.

A project deleted outside the app still has no run and no history; the UI must
not pretend otherwise.

### Interaction with rows

A project deletion is one run with both sides: file, directory and link entries
for the whole folder plus a row diff for `projects`, `project_folders`,
`project_paths`, `boards`, `columns` and `cards`. Undo recreates the directory
tree, writes the files and links, then restores the rows from the start snapshot,
so the project row lands on an existing directory.

### Size and retention

`PROJECT_FILE_EXTS` includes `pdf`, images, `docx`, `xlsx` and `pptx`
([L114-L117](file:///Users/zefang/Documents/career/airs/general/kandrium/crates/core/src/projects.rs#L114-L117)),
so a project folder can hold large uploads even though the app's own upload path
accepts only PDF/Markdown. The shared 256 MB per-file cap and 2 GB total apply.
Preflight the tree's total size and entry count before capture. Capture may prune
older prunable runs to make room (implicit first, then app, then explicit beyond
its minimum), but never the run being written and never an `open` or
`files-restored` run. If the tree still cannot fit, capture every entry that
fits and mark only the overflow `restorable: false` with `reason: "budget"`, so
the run is partial only by that residue; the delete itself is never refused for
budget alone. The delete confirmation shows the estimate. `create` entries need
no pre-image, so imports are cheap regardless of source size; only a later
delete pays the snapshot cost.

---

## 3B. Row capture: mechanism choice

### Rejected alternatives

**A. Generic row journal written by every core function.** Every mutating
function records before/after JSON per row. Rejected: dozens of functions across
`projects.rs`, `services/*.rs`, `clocks.rs`, with nesting (`create_project`
also creates a board and columns), so missing one write silently breaks undo.

**B. Trigger journal.** A trigger per table writes `OLD` rows into a change log.
Rejected: SQLite foreign-key actions do not fire triggers, so cascade-deleted
children are lost unless every cascade is reimplemented; plus one trigger per
table and a run-context table.

**C. SQLite session changesets.** Row-level before/after capture with native
inversion. Rejected for this codebase: the `session` feature implies
`libsqlite3-sys/preupdate_hook` and `buildtime_bindgen`
([libsqlite3-sys Cargo.toml L68-L72](file:///Users/zefang/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/libsqlite3-sys-0.38.2/Cargo.toml#L68-L72)),
which adds a libclang/build-time requirement for a 172 KB database. Sessions are
also per connection, so a run spanning the app and MCP would need multiple
segments and inversion order handling. The benefit does not justify the cost at
this size.

**D. Per-run database snapshots plus a table diff.** Chosen. No new dependency,
pure `rusqlite`/SQLite, catches cascades because the snapshot contains every row,
and is trivially cheap at this database size.

### Chosen design

A run takes two snapshots and derives the row changes between them:

1. **Start**: `VACUUM INTO <history>/db/<run-id>-start.db` before the run's first
   write. Outside any transaction.
2. **End**: `VACUUM INTO <history>/db/<run-id>-end.db` after the last write.
3. **Diff**: compare start vs end over the allowlist, producing
   `db/<run-id>-rows.json` with per-table key lists and row hashes.
4. **Delete the end snapshot** after a successful diff. Keep the start snapshot
   and the diff. The end values are represented by hashes, which is all that undo
   conflict detection needs. If the diff fails, keep the end snapshot and mark
   the run `snapshot-only` for retry.

Undo compares the stored after hashes with the current rows to find rows edited
after the run, then restores the start-state rows. Files and rows share one run
id and are preflighted together (§4).

### Row identity

- Use the declared primary key columns. All eleven allowlisted tables have one;
  `all_card_order` uses `card_id`, `area_card_order` uses
  `(board_id, card_id)`, the rest use `id`.
- Fallback for future tables without a PK: `rowid`. No allowlisted table needs it
  today; `wiki_fts` is a virtual table and is excluded.
- A key present only in the start snapshot is a **deleted** row and is restored
  by insert.
- A key present only in the end snapshot is an **inserted** row and is removed
  on undo.
- A key present in both with a different non-key hash is an **updated** row.
- A primary-key change appears as a delete plus an insert. This is deliberate.

### Table allowlist

Captured: `boards`, `columns`, `cards`, `all_card_order`, `area_card_order`,
`projects`, `project_folders`, `project_paths`, `clocks`, `clock_people`,
`clock_travels`.

Excluded: `wiki_pages`, `wiki_page_sources`, `wiki_links`, `wiki_sources`,
`wiki_runs`, `wiki_meta`, `wiki_fts` and its shadow tables, and every
`sqlite_*` table. There is no database settings table; `clocks_use_24h` and the
rest live in `gpui-settings.conf`, which is out of scope.

**Why the wiki index is excluded**: file restore followed by
`index::reindex_page` / `write_index_md` is already the authoritative rebuild.
Restoring index rows as well would create two competing undo paths. The rule is:
**files own the wiki; rows own the app entities.**

### Ordering, foreign keys, cascades

Verified schema:

- `columns.board_id → boards` CASCADE
- `cards.column_id → columns` CASCADE; `cards.project_id → projects` SET NULL;
  `cards.area_board_id → boards` SET NULL
- `projects.board_id → boards` CASCADE; `projects.folder_id → project_folders`
  CASCADE
- `project_paths.project_id → projects` CASCADE
- `project_folders.parent_id → project_folders` CASCADE
- `all_card_order.card_id → cards` CASCADE; `area_card_order` cascades from both
  boards and cards
- `clock_people.home_clock_id → clocks` RESTRICT

Restore procedure:

1. `PRAGMA foreign_keys = OFF` **before** the transaction (the pragma is a no-op
   inside one).
2. `BEGIN IMMEDIATE`.
3. For every key in the diff, in any order (FKs are off):
   - inserted key: `DELETE FROM <table> WHERE <pk> = ?`.
   - updated key: delete that key, then insert its start-snapshot row.
   - deleted key: insert its start-snapshot row.
   Only keys in the diff are touched; later runs' rows in the same table survive.
4. `PRAGMA foreign_key_check`; any row aborts and rolls back.
5. `COMMIT`, then `PRAGMA foreign_keys = ON`.

Why not `defer_foreign_keys` alone: it defers FK checks but not `UNIQUE`
constraints, and `projects.identifier` is uniquely indexed. Restoring one key at
a time as delete-then-insert, with foreign keys off, avoids transient unique
swaps within the run's own key set; deletes do not cascade. A unique collision
with a later run's row fails the insert, rolls back and is reported as a
conflict. **Whole-table replacement is rejected**: with many small interleaved
runs, it would revert later runs' untouched rows in the same table.

**Cascade recovery is the reason this beats a trigger journal**: the start
snapshot contains the child rows physically deleted by `ON DELETE CASCADE`, so
the diff reports them as deletes and undo reinserts them. No trigger needs to
observe the cascade.

### Run boundaries

Undoable units are task-sized batches, not connections.

- **Explicit run (agent)**: `begin_wiki_run { label }` opens a run and returns its
  id. The label is the task name shown in History ("ingest the attention
  papers"). Opening writes the single `open` row in `run_undo`; the unique
  partial index (§2) makes a second open row impossible. The start snapshot is
  taken lazily, immediately before the first row mutation, not at `begin`, so
  reading time is outside the window. Every write call while the run is open
  joins it. `end_wiki_run { note? }` takes the end snapshot, derives the diff and
  closes the run.
- **One open run**: the open row lives in the database, so every process sees it.
  A second MCP client that calls `begin_wiki_run`, or any mutating tool, while a
  fresh run is open gets `a run is open: <label>` and is refused; it never joins.
  Read-only tools remain available. The app is not a second client: its
  per-action runs are inserted `committed` at close, so user edits are never
  refused and an edit overlapping an explicit run marks that run `contended`.
- **Heartbeat**: `begin_wiki_run`, every run-scoped MCP call and
  `heartbeat_wiki_run { runId }` advance `last_heartbeat_at` (mirrored as
  `heartbeatAt`). Ten minutes without a heartbeat makes the run reclaimable: the
  next `begin_wiki_run` marks it `abandoned` in the same transaction that opens
  its successor. There is no separate staleness rule. If the original owner
  returns after a reclaim, its mutating calls are refused like any second
  client, and its `end_wiki_run` fails with `run was reclaimed`.
- **Implicit run**: if no run is open, each mutating MCP tool call is its own
  run. Start and end snapshots are taken around the call; the label is derived
  from the tool and target (`write_wiki_page concepts/attention`,
  `create_card`). Read-only tools never open a run. Implicit runs are inserted
  `committed`; they never hold the open slot. This is the default and is
  fine-grained; grouping is opt-in.
- **App run**: unchanged — one mutating `Store` method is one run, labelled by
  the action (`archive project`, `move card`), with `kind: "app"`, inserted
  `committed` at close.
- **Nesting**: `begin_wiki_run` while a run is open is an error reporting the
  open label. Runs do not nest.
- **Tool surface**: `begin_wiki_run`, `end_wiki_run` and `heartbeat_wiki_run`
  are additive. `record_wiki_run` remains for compatibility: when a run is open
  it closes it and records the supplied stats; when none is open it records a
  ledger-only run with no snapshots.
- **Process exit**: the server attempts `end_wiki_run` on shutdown. If it cannot
  (signal, crash), the open row survives and its heartbeat stops. After 10
  minutes another client can reclaim it; the 24-hour sweep over
  `last_heartbeat_at` marks it `abandoned` even if no new run starts. An
  abandoned run has no diff, so its rows are not undoable; its captured files
  remain undoable.
- **Undo while a run is open**: refused ("close the open run first"). Undoing one
  implicit run in the middle of a longer agent task is allowed only when no later
  run touched the same keys; independent later work is untouched because restore
  is per-key.

### Contention

- `PRAGMA data_version` is read on the run connection before the start snapshot
  and after the end snapshot. A change means another connection committed during
  the window.
- Implicit runs are short (one tool call), so contention becomes rare and almost
  always indicates a real concurrent edit.
- Explicit runs can still be long: an agent may read for minutes or think between
  writes, and the lazy start still spans all writes until `end`. App edits in that
  span are allowed and mark the run `contended`.
- A second MCP client cannot add contention: it is refused while the run is open.
- Default remains refuse-unless-forced. The flag is strong evidence that the
  start snapshot is not the state the user saw, and a per-key restore could undo
  someone else's write.

### Cost, measured on this database

- Database: 172 KB plus 109 KB WAL; `VACUUM INTO` snapshot: 268 KB.
- Two snapshots: **0.02–0.05 s each**, measured with the `sqlite3` CLI.
- Full diff of the eleven allowlisted tables (about 228 rows) with per-row
  hashes: **~5 ms** in Python; the Rust implementation will be comparable or
  faster.
- Retention at task-sized runs: one start snapshot per run. 20 explicit runs
  ≈ 5.4 MB. Implicit runs are many and small: 200 retained × 268 KB ≈ 54 MB.
  Deleting the end snapshot after the diff keeps this bounded.
- **Implicit path cost**: a mutating MCP call pays two snapshots (≈536 KB
  transient, 40–100 ms total) plus a ~5 ms diff. A 50-write agent session therefore
  spends 2–5 s and about 27 MB of transient write churn, with one retained
  snapshot per call. This is acceptable at the current size; it is the reason the
  switch threshold exists.
- Read-only tools and no-op calls: read-only tools never snapshot; a mutating tool
  that changes nothing still pays the pair and produces an empty diff. Do not add
  post-hoc mutation detection; it would require the incremental capture this
  design avoids.

**Switch threshold**: if the database exceeds roughly 32 MB, if a snapshot takes
over 500 ms, or if snapshot size × retained runs exceeds 1 GB, move to an
incremental mechanism (a row journal or session changesets). None of those
conditions is close at 172 KB. Record the measured numbers in the run manifest so
the threshold can be re-evaluated from real data.

### Schema drift

- The manifest records `user_version` and a schema hash of
  `PRAGMA table_info` for every allowlisted table.
- At undo, if `user_version` differs, or any allowlisted table is missing or its
  hash differs, **refuse** with `SchemaDrift`. Do not attempt to map columns
  across migrations.
- A run whose start and end snapshots have different `user_version` values is
  marked non-undoable at run end (a migration happened mid-run).
- Suggested default: no override. The escape hatch is the existing database
  snapshot/backup, not a best-effort row restore.

### What is unundoable under this design

- Writes made outside any run window (direct `sqlite3` edits, older binaries,
  seed/bootstrap, a crash before the start snapshot).
- Writes by another connection during the window: detected as `contended` and
  refused by default.
- DDL and migrations, including a migration between run and undo.
- Any table outside the allowlist, including all derived wiki tables and FTS.
- Settings in `gpui-settings.conf`.
- Entries marked `restorable: false`: files above the 256 MB cap, unreadable
  files, special files, or entries the budget could not fit. They are listed by
  path and reason; they are the only reason a run is `partial`.
- A run whose start snapshot, diff, or manifest is missing or corrupt.

---

## 4. Restore semantics (files and rows)

`undo_run(run_id, force)`:

1. Load the manifest; refuse if `state` is `open` or `undone`. A `committed`,
   `partial` or `files-restored` run proceeds; `files-restored` repeats only the
   row phase. An `abandoned` run has no diff, so it runs the file phase only and
   reports rows as not undoable.
2. Preflight file paths against §5 and the row diff against the snapshot conflict
   rules (including the per-table schema hashes, `user_version` and
   `contended`). A real conflict with `force` false aborts before touching
   anything and returns the conflict list. Entries already marked
   `restorable: false` are not conflicts; they are returned as the expected
   `partial` list.
3. Take the pre-undo database backup (temp file).
4. Recreate directories the run removed (`dirs` op `rmdir`), parent-first,
   including empty ones, with the recorded mode. A file at a directory path is a
   conflict; an existing non-empty directory is also a conflict.
5. Restore files and links in reverse `seq` order:
   - `modify`: write before bytes through `fsutil::write_atomic`, restore mode.
   - `create`: delete the path. Directories the run created are handled in step 6.
   - `delete`: write before bytes back; restore mode.
   - `link`: recreate the symlink with the recorded target.
   - `restorable: false` or a missing object: skip that entry and list its path
     and reason in the report. Normal undo does not abort for an entry that was
     already known to be unrecoverable; the run finishes `partial`.
   - Project paths are re-joined under the current `KANDRIUM_PROJECTS` root; a
     missing project row produces an orphan-folder report, not an error.
6. Remove directories the run created (`dirs` op `mkdir`), child-first, only when
   empty; a non-empty directory is a conflict instead of deleting user content.
7. Restore rows from the start snapshot inside one transaction (skip when the
   run is `abandoned` or `rows.restorable` is false):
   - `PRAGMA foreign_keys = OFF` before `BEGIN IMMEDIATE`; `BEGIN IMMEDIATE`.
   - For every key in the diff: delete inserted keys, delete-and-reinsert updated
     keys from the start snapshot, insert deleted keys. Only those keys are
     touched; later runs' untouched rows remain.
   - `PRAGMA foreign_key_check`; any row aborts and rolls back.
   - `COMMIT`, then `PRAGMA foreign_keys = ON`.
   - On any error without `force`: roll back; file restores already done stay,
     and state becomes `files-restored` for retry.
   - A row conflict aborts the whole run before files are touched because §4
     step 2 preflights it.
8. Reconcile derived state once:
   - Wiki paths: `index::reindex_page` / DB-only `index::forget_page`, then
     `index::write_index_md`. Never call `write::delete_page` from restore.
   - Documents: `sources::refresh_sources`.
   - Notes: nothing.
   - Delete the pre-undo backup on success.
9. Mark `run_undo.state = 'undone'`, or `partial` when entries were skipped,
   with `undone_at`.
10. Return a report: files restored/deleted/skipped/conflicted, directories
    recreated/removed, rows updated/inserted/deleted/conflicted.

`index.md` is excluded from capture and always regenerated. The source table is
never restored from history; `refresh_sources` makes it match restored docs.
`wiki_runs` is historical and never rewritten.

Idempotency: second undo returns `AlreadyUndone`; retry from `files-restored`
runs only the row phase and reconcile; an `open` run is refused with "run still
in progress".

---

## 5. Conflict handling

### Files

Refuse when, for any entry: `modify` and current hash differs from `after_hash`
or the file is missing; `create` and current content differs from `after_hash`;
`delete` and the path exists with a hash other than `before_hash`; a `dirs` entry
with op `rmdir` and a file occupies the path or the directory is non-empty; a
`dirs` entry with op `mkdir` and the directory is non-empty; a `links` entry and
the path exists with a different target or is not a symlink; or a later run
recorded an entry for the same path. Entries with `restorable: false` are
reported, not treated as conflicts. Project entries use the same rules under the
`projects` root.

### Rows

For each key in the derived diff, compare the current row with the end state:

- **Updated key**: current row must exist and hash equal the diff's after hash.
  Missing or different is a conflict.
- **Inserted key** (present only in end): current row must exist and match the
  after hash. Missing or different is a conflict.
- **Deleted key** (present only in start): the key must be absent now. Present is
  a conflict, even if the values match the start row.
- **Contended run**: `data_version` changed during the window, so another
  connection wrote. Treated as a run-level conflict; `force` is required.
- **Schema drift**: `user_version` or an allowlisted table's schema hash
  differs. This is a hard refusal, not overridable by `force`.

`force` restores the start-snapshot values for every key in the diff, overwriting
row conflicts and skipping conflicted files; it never deletes files. The conflict
report lists table, primary key and reason. A run with no row changes has an empty
diff and restores files only. A small run undone in the middle of a task touches
only its own keys; later runs' independent rows survive, and overlapping keys
conflict.

Message shape:

```
Cannot undo run <id>: 2 files and 1 row changed after the run.
  modified  concepts/attention.md
  missing   sources/kong.md
  row       cards <id> changed
  later run cards <id2> (run <later-id>)
Re-run with force to replace rows and skip these files.
```

---

## 6. Retention and disk cost

- **Dedup**: file objects are content-addressed. Row snapshots are per-run and
  not shared; the derived diff is small (keys and hashes only).
- **Prune by kind**: explicit agent runs newest 20 and 30 days; implicit runs
  newest 200 and 7 days; app runs newest 100 and 3 days. Prune implicit first,
  then app, then explicit. Never prune `open`, `files-restored` or the run being
  written. Enforce a shared 2 GB ceiling including file objects, start snapshots
  and directory/link entries.
- **Prune to fit a directory delete**: a project or folder delete may prune older
  prunable runs (in the same order, down to the minimums above) to make room for
  its tree. If the tree still cannot fit, capture what fits and mark only the
  overflow entries `restorable: false`; the delete is not refused for budget
  alone. The confirmation names the estimate.
- **GC**: unreferenced objects and start snapshots removed after pruning;
  crash-orphan blobs collected by the same pass. Delete `db/<run-id>-*` with the
  manifest.
- **Large files**: APFS clone when available, else copy; 256 MB cap. Above the
  cap mark `oversize: true`, `restorable: false`, `reason: "oversize"`; the
  History entry lists the path. Unreadable and special entries carry the same
  markers. `create` entries need no pre-image; directory and symlink entries are
  tiny.
- **Row cost**: measured 268 KB per snapshot, two per run while diffing, one
  retained; a full diff is ~5 ms. Retention of 20 runs is ≈5.4 MB of snapshots
  plus small JSON. The 172 KB database makes snapshots cheaper than maintaining
  an incremental journal; revisit at the §3B switch threshold.
- **Estimate**: 173 Markdown notes are ~5 MB; 20 retained agent runs stay in the
  low hundreds of MB even with PDF deletes, which dominate. A directory delete
  preflights its total size and entry count before capturing, prunes older runs
  when needed, and reports any overflow rather than silently consuming the
  budget.

---

## 7. Surfaces

### App

- **History** panel beside Status/Lint/Setup. Runs are grouped by task label:
  explicit labels as written by the agent, implicit runs as
  `write_wiki_page concepts/attention`, app runs by action. Each row shows kind,
  time, file/directory/link counts, row count, state and whether undoable. It
  lists project document runs too; the project detail pane gets a History action
  that opens the panel filtered to that project.
- The open run is pinned at the top with its label, `open` state and heartbeat
  age ("heartbeat 12 s ago"). It cannot be undone; the panel offers no action
  other than inspecting it.
- Selecting a run shows a diff preview: text files as before/after, binary files
  as names and sizes, directories as a tree, rows as table and key counts. A
  directory delete previews the full tree. Entries with `restorable: false` are
  listed by path and reason, and the entry reads "partial" only when they exist.
- **Undo run** warns when a later run touched the same keys ("3 later runs depend
  on these rows") and refuses if an explicit run is still open.
- After undo: reload the current section. The watcher already reacts to
  `data_version` and `apply_external_change` reloads notes, documents, board,
  archive, clocks and projects; the app also reloads explicitly after its own
  undo. Open card modals, title editors, add-card drafts and project dialogs are
  dismissed if their rows no longer exist, otherwise reloaded.
- If a drag is active, undo is refused with "finish the drag first" rather than
  mutating state mid-gesture.

### MCP

Additive tools only; the 13 Kanban tool schemas stay untouched:

- `begin_wiki_run { label }` — opens an explicit task run; returns the run id.
  Refused with `a run is open: <label>` while a fresh run is open.
- `heartbeat_wiki_run { runId }` — advances `last_heartbeat_at`; intended for
  agents that spend a long time outside MCP between writes.
- `end_wiki_run { note? }` — closes the open run, snapshots, derives the diff.
- `list_runs` — recent runs grouped by label, with file/directory/link and row
  counts, heartbeat age, state and whether undoable.
- `undo_run { runId, force? }` — restores files, directories and rows, returns
  the report.

Any mutating tool from a client that does not own the open run is refused with
`a run is open: <label>`; read-only tools stay available. `record_wiki_run`
remains: it closes an open run when one exists (recording the supplied stats) and
otherwise records a ledger-only entry without snapshots. The 13 Kanban tool
schemas are untouched and join the open or implicit run.

Tool count moves from 26 to 31; the router test pins the new sorted list.

### Ledger

`wiki_runs` remains the ledger; `run_undo.state` and
`run_row_changes` are joined for display. The Status panel shows the latest run
and whether it is undoable. `stats_json` is unchanged.

---

## 8. Staged implementation

Existing stages keep their numbers; lettered stages are inserted at their
dependency point. Each stage ends with `cargo fmt --all`,
`cargo clippy --workspace --all-targets`, and `cargo test --workspace` clean.

**Stage 0 — schema and types.** Add `0013_run_history.sql` with `run_history`
(`kind`, `target`, `empty`, `reason`), `run_row_changes`, `run_undo`
(`last_heartbeat_at`) and the partial unique index `idx_run_undo_one_open`;
register it; add manifest structs for file, directory and link entries plus row
snapshot/diff metadata. Verify: v12 database migrates to 13; fresh boot reaches
13; serde round-trip tests; a second `open` row is rejected by SQLite while a
`committed` row coexists.

**Stage 1 — file object store and manifest writer.** `history::capture` /
`commit` / `load` / `prune`. Verify: dedup, crash-orphan blobs, manifest
round-trip.

**Stage 2 — wiki file writes.** Plumb `run_id`; capture in `write_page` and
`delete_page`; `patch_page` inherits. Verify: overwrite/create/delete entries;
`index.md` never appears; failed `expectedHash` writes no history.

**Stage 2a — snapshot capture and table diff.** Add `history::row_begin` /
`row_end`: the start snapshot is taken lazily at the first row mutation of a run,
recording `user_version`, allowlist schema hash and `PRAGMA data_version`; at end
take the end snapshot, diff the allowlist, write `db/<run-id>-rows.json`, store
counts, label/kind and the contended flag, then delete the end snapshot. Advance
`last_heartbeat_at` at run begin and at each row phase. Verify: snapshot size and
timing; insert/update/delete detection; a board delete yields child deletes for
columns, cards and both order tables; end snapshot removed after diff; a failed
diff keeps the end snapshot and marks the run `snapshot-only`; a run with no
mutations produces no snapshot; two-connection test sets `contended`.

**Stage 3 — documents and notes file capture.** Import, document delete,
annotations, Markdown write/create/rename, notes write/rename/delete. Add
`history::capture_tree` and route `notes::delete_path` and
`documents::delete_path` through it before `fs::remove_dir_all`: files,
directories (including empty), symlink targets, and unrestorable reasons.
Verify: sidecar capture; no entry for unchanged bytes; a recursive directory
delete captures the whole tree before removal; a delete interrupted mid-walk
still has a complete manifest; no unguarded `write_atomic` in these modules.

**Stage 3a — project document capture.** Wire `projects::write_doc`,
`create_doc_file`, `create_doc_folder`, project-root `import_file`, and the same
`capture_tree` in `delete_project` / `delete_folder` before the SQL transaction.
Verify: save/create/mkdir/upload entries; project delete captures every file,
directory and link before the SQL transaction; an oversized or unreadable file
is the only thing that makes the run `partial` and History lists it with its
reason; rename, identifier change, archive and folder move produce no file
entries and keep the same manifest path.

**Stage 4 — file restore engine.** §4 directory and file phases, `dirs` and
`links`, and §5 file conflicts. Verify: directories recreated parent-first
including empty ones; modify/create/delete restore; `reviewed` survives;
`index.md` regenerated; a project delete restores the full directory tree and
links; a skipped `restorable: false` entry leaves the run `partial` while every
other entry restores; an orphan project folder is reported when its row is gone.

**Stage 4a — unified undo from snapshots.** Add the row phase: schema-drift and
contention refusal, `foreign_keys=OFF`, per-key delete/reinsert from the start
snapshot, `foreign_key_check`, pre-undo backup, and one reconcile pass; make
`undo_run` atomic in intent. Verify: a mixed run (import + page write + card move
+ project delete) undoes all sides; a board delete restores cascaded children; an
identifier swap restores without a unique-index failure; a later run's untouched
rows in the same table survive; a row conflict aborts before files change; schema
drift refuses even with `force`; retry from `files-restored`; second undo is
idempotent.

**Stage 5 — MCP file tools.** `list_runs`, `undo_run`, optional `runId` on wiki
writes. Verify: write→patch→delete undoes as one run; `KANDRIUM_MCP_WIKI=read`
refuses writes and undo; router pins 28 names; 13 Kanban schemas unchanged.

**Stage 5a — task, implicit and heartbeat run boundaries.** Add `begin_wiki_run`,
`end_wiki_run` and `heartbeat_wiki_run`; enforce the single open run through the
unique partial index; refuse a second MCP client with `a run is open: <label>`;
open an implicit run per mutating call when none is open; insert implicit and app
runs `committed`; make `record_wiki_run` close an open run or record a
ledger-only entry; auto-close on shutdown; reclaim after 10 minutes without a
heartbeat and mark abandoned on the 24-hour sweep. Verify: two explicit runs
produce two labelled undo entries; a run of three writes is one entry; without
`begin` each mutating call is its own entry and stays undoable; read-only calls
create none; nested `begin` errors; a second client is refused while a run is
open; a killed process leaves an `open` row that a later `begin` reclaims after
10 minutes; the 24-hour sweep marks it `abandoned` with files undoable and rows
not; an app write during an explicit run is allowed and marks it `contended`;
router pins 31 names.

**Stage 6 — app History panel for files.** List, detail including directories and
links, undo confirmation, reload. Verify with a temp-root headless test.

**Stage 6a — app run wrappers and row display.** Wrap mutating `Store` methods so
each action has a short snapshot window and inserts its run `committed` at close;
show row counts and table names; dismiss stale modals. Verify: create project
(folder + board + columns) undoes as one run; card drag undo restores column,
position and `all_card_order`; clocks revert; a project directory delete made
while an explicit run is open is allowed, fully captured (files, directories,
links, rows) and recorded as its own app run, while the explicit run is marked
`contended`; watcher generation bumps and the UI reloads.

**Stage 7 — retention and GC.** Prune runs, file objects, start snapshots and
diffs; cap; directory-delete preflight and prune-to-fit; oversized handling.
Verify with synthetic timestamps, a shared-object case, and a tree large enough
to force pruning while smaller entries still capture.

**Stage 8 — docs and agent contract.** Bump `WIKI_INSTRUCTIONS_VERSION` to 2.
Replace the `record_wiki_run` workflow step in `instructions.md` with
`begin_wiki_run` (short task label), writes, `heartbeat_wiki_run` during long
reads, then `end_wiki_run`; document that only one run can be open, a second
client is refused, and without `begin` each write is its own undo entry. Keep
`record_wiki_run` described as the legacy close. Verify: instruction drift test
covers `begin_wiki_run`, `heartbeat_wiki_run`, `end_wiki_run`, `list_runs` and
`undo_run`; a v1 agent's write is rejected by the existing `ensure_version` gate
until it refetches, and the error says so; the Setup panel shows version 2.

---

## 9. Risks

- **Snapshot cost and contention**: two `VACUUM INTO`s per run and a full-table
  diff. Measured at 0.02–0.05 s and 268 KB per snapshot here, but the cost grows
  with the database and a long MCP window can overlap app edits. Mitigation:
  short app windows, `contended` marking, the §3B switch threshold.
- **Bypass**: future code could call `fsutil::write_atomic` directly or open a
  connection without a run wrapper. Mitigation: a test enumerating allowed
  `write_atomic` sites, and a single `Store`/MCP wrapper for row runs.
- **Excluded tables**: a future app table must be added to the allowlist or it is
  silently not undoable. Mitigation: a test compares the allowlist against
  `sqlite_master` and fails on unknown non-wiki tables.
- **Crash ordering**: the start snapshot is taken before the first write; the end
  snapshot and diff are taken at run end. A crash leaves the start snapshot and
  no diff, so the run shows as non-undoable rather than half-restorable.
- **Concurrency**: app and MCP are separate processes. Manifest rewrites need a
  per-run lock file; `run_row_changes` inserts use `BEGIN IMMEDIATE`; a foreign
  connection write during a window sets `contended`. The unique partial index is
  the single-open-run guarantee; app runs insert `committed` at close, so a user
  edit never collides with the open slot.
- **Row restore mechanics**: `foreign_keys=OFF` plus per-key delete/reinsert
  avoids ordering and unique-swap failures; `foreign_key_check` before commit is
  the safety net. A violation or cross-run unique collision rolls back the whole
  row phase.
- **Many small runs**: implicit calls create one run each, so the ledger and
  retention budget grow quickly (≈268 KB per run, 2–5 s per 50 calls). Retention
  is split by kind and implicit runs are pruned first; the switch threshold caps
  the worst case.
- **Abandoned runs**: a crash leaves the open row without a diff. Rows cannot be
  undone; files can. The row is reclaimable after 10 minutes without a heartbeat
  and is marked `abandoned` by the 24-hour sweep. The UI must show `abandoned`
  plainly rather than offering a broken undo.
- **Heartbeat false expiry**: an agent that reads for more than 10 minutes
  without an MCP call looks dead, and another client can reclaim the run.
  Mitigation: refresh on every run-scoped call, `heartbeat_wiki_run` for long
  external work, and reclaim marks the old run `abandoned` (its files stay
  undoable) instead of deleting it.
- **Second-client refusal**: while a run is open, another MCP client cannot
  write. The error names the open label; read-only tools keep working. This is
  intentional and must be documented in the instructions, not treated as a bug.
- **Mid-task undo**: undoing one small run inside a longer agent task restores
  only its keys. Later runs depending on those keys conflict; independent rows
  survive. There is no dependency graph, so the report must name the later runs.
- **Schema drift**: any migration makes older runs non-undoable by design. The UI
  must say so rather than attempt column mapping.
- **Watcher churn**: wholesale restore bumps `data_version`, causing a reload of
  every section; acceptable for an explicit undo. Refuse undo mid-drag.
- **Directory delete volume**: a folder delete captures every file, directory and
  link before removal, so it can be large. Preflight, prune older prunable runs
  to fit, and mark only the overflow entries `restorable: false`; the History
  entry lists them by path and reason. The delete is never refused for budget
  alone.
- **Directory walk races**: a file can vanish between enumeration and capture.
  Record it as `unreadable`/missing rather than aborting the whole delete; the
  manifest stays complete for everything that was present.
- **External project edits**: files added or changed in Finder are not captured
  until a captured delete removes them; a `modify` restore refuses on hash
  mismatch.
- **Referenced folders**: `project_paths` point outside the projects root and are
  never captured; only the reference row is undoable.

---

## 10. Decisions and open questions

### Settled decisions

1. **Implicit per-call snapshots: accepted.** A mutating MCP call with no open run
   takes two snapshots, keeps the start snapshot and the diff, and deletes the
   end snapshot. There is no coalescing window: one call is one undo entry. The
   §3B switch threshold (32 MB database, 500 ms snapshot, 1 GB retained) is the
   documented point at which this is revisited, not a reason to group calls now.
2. **One open run, stored in the database.** The open run is a `run_undo` row,
   and a unique partial index on `state = 'open'` makes a second open row
   impossible. A second MCP client is refused with `a run is open: <label>`; it
   never joins. `last_heartbeat_at` is the single staleness signal: `begin`,
   every run-scoped call and `heartbeat_wiki_run` refresh it, 10 minutes without
   a heartbeat makes the run reclaimable, and the 24-hour abandonment sweep uses
   the same field instead of a separate stale check. The app is not a second
   client: its per-action runs are inserted `committed` at close, so user edits
   are never refused and overlap marks the open run `contended`.
3. **Directory deletes recover files and rows.** Every recursive delete
   (`notes::delete_path`, `documents::delete_path`, `projects::delete_project`,
   `projects::delete_folder`, any future wiki-folder delete) enumerates the tree
   before removal through one helper: a pre-image per file, a directory entry per
   directory including empty ones, and a link entry per symlink. Restore recreates
   directories, writes files and links, then restores rows. A delete is allowed
   while a run is open and is recorded as its own app run, fully captured.
   `partial` is reserved for entries that cannot be captured — oversize,
   unreadable or special files — and History lists each by path and reason.

### Open questions and suggested defaults

1. **Row mechanism.** Default: start/end `VACUUM INTO` snapshots plus a
   table-diff. Switch to an incremental journal only past the §3B threshold
   (32 MB database, 500 ms snapshot, or 1 GB retained).
2. **End snapshot.** Default: delete it after a successful diff; keep it and mark
   `snapshot-only` if the diff fails. Start snapshot and diff are retained.
3. **Start snapshot failure.** Default: do not block the user action; record the
   run with `rows.restorable = false` and warn that only files are undoable.
4. **Explicit run start.** Default: `begin_wiki_run` records the label and opens
   the run; the snapshot is taken lazily at the first row mutation so reading
   time is outside the window. A run with no writes closes with no snapshot.
5. **Tool surface.** Default: add `begin_wiki_run`, `end_wiki_run` and
   `heartbeat_wiki_run`; keep `record_wiki_run` as a compatibility close (with
   stats when a run is open) and a ledger-only entry when none is open.
6. **Process exit.** Default: attempt `end_wiki_run` on shutdown; a surviving
   open row is reclaimable after 10 minutes without a heartbeat and is marked
   `abandoned` by the 24-hour sweep. Abandoned runs expose file undo only.
7. **Undo while a run is open.** Default: refuse; require `end_wiki_run` first.
8. **Mid-task undo.** Default: allow undoing an implicit run; restore only its
   keys; refuse on key overlap with later runs and name those runs in the report.
9. **App run boundary.** Default: unchanged — one run per mutating `Store` method,
   labelled by the action, inserted `committed` at close.
10. **Kanban tools.** Default: do not change their schemas; they join the open or
    implicit run.
11. **Contention.** Default: mark `contended` and refuse undo; `force` allows it
    with a warning. Short implicit windows make it rare; explicit runs can still
    span minutes, while a second MCP client is refused rather than contending.
12. **Schema drift.** Default: refuse undo when `user_version` or an allowlisted
    table shape differs; `force` does not override.
13. **Row conflict policy.** Default: abort the whole undo; `force` restores the
    start-snapshot values for the run's keys and skips conflicting files.
14. **User-action undo in the UI.** Default: yes, shown as "Your actions" with a
    confirmation; same retention as app runs.
15. **Directory capture budget.** Default: preflight size and entry count, prune
    older prunable runs to make room (implicit first, then app, then explicit
    beyond its minimum), and capture what fits. Overflow entries are marked
    `restorable: false` and listed; the delete is never refused for budget alone.
16. **Captured tables.** Default: the eleven-table allowlist; never include
    `wiki_*` or FTS.
17. **Pre-undo database backup.** Default: always, temp file deleted on success.
18. **Retention.** Default: explicit 20 runs / 30 days, implicit 200 runs /
    7 days, app 100 runs / 3 days; shared 2 GB cap; prune implicit first.
19. **Settings.** Default: `gpui-settings.conf` is out of scope; it is not under
    a captured root.
20. **Legacy runs.** Default: runs captured before history existed are not
    undoable and show as such in the ledger.
21. **Projects root in scope.** Default: yes, captured per operation; no periodic
    scan.
22. **Directory entries and empty folders.** Default: record every directory with
    its mode and `empty` flag, including empty ones, because the docs tree shows
    and the project shape depends on them.
23. **Missing project row at undo.** Default: recreate
    `<projects_root>/<project_id>/` and report an orphan folder; do not delete the
    restored files.
24. **External project edits.** Default: not captured; undo refuses on hash
    mismatch rather than overwriting.
25. **Per-document delete/rename.** Default: no such feature exists; when added it
    must be a core function with capture, and a test must reject unguarded
    `fs::remove_file` / `fs::rename` under the projects root.
26. **Project upload types.** Default: the app imports only PDF/Markdown; other
    file types found in project folders are external and are captured when a run
    deletes them.
27. **Changed projects root.** Default: restore under the current
    `KANDRIUM_PROJECTS` and flag the root change in the report.
28. **Instruction version.** Default: bump to 2 with `begin_wiki_run`,
    `heartbeat_wiki_run` and `end_wiki_run`. The existing `ensure_version` gate
    rejects a v1 write until the agent refetches; a v2 agent that omits `begin`
    still works through implicit runs.
29. **Heartbeat tuning.** Default: 10-minute expiry, refreshed by every
    run-scoped call and `heartbeat_wiki_run`. Revisit from real logs if agents
    routinely think for longer without calling a tool.
30. **Symlinks and special files.** Default: capture symlinks by target and
    recreate them; FIFOs, sockets and devices are `special`, not restorable, and
    make the run `partial`.
31. **Policy exclusions.** Default: `.DS_Store`, temp files, `index.md` and the
    history root are not captured and do not make a run partial; only genuine
    user content that cannot be captured does.

---

## What changed in the earlier drafts

The file-history design is intact. Changes required by the row side:

1. **Manifest** gains a `rows` object; file entries move under `files`.
2. **Migration 0013** gains `run_row_changes` with snapshot/diff metadata.
3. **Undo** becomes ordered: directories, then files and links, then rows for
   only the run's keys, then one derived-state reconcile. The file phase no
   longer reindexes immediately.
4. **MCP tool names** generalize from `list_wiki_runs` / `undo_wiki_run` to
   `list_runs` / `undo_run`, because scope is no longer wiki-only.
5. **History panel** shows row counts and table names.
6. **Row capture** is now start/end snapshots plus a table diff. No `rusqlite`
   feature, no bindgen, no libclang build requirement.

Changes required by the projects root:

7. **File scope** adds `root: "projects"` with
   `<project_id>/<rel_path_inside_project>` keys, plus `dirs` and `links`
   entries for directories and symlinks.
8. **Directory deletion** enumerates the whole tree before removal and is fully
   undoable; `partial` is only the residue of entries that cannot be captured
   (oversize, unreadable, special).
9. **A new Stage 3a** adds project document capture; Stage 4 gains directory and
   link restore and orphan-folder reporting. Row stages 2a, 4a, 5a and 6a now use
   snapshots and a table diff instead of session changesets.

Changes required by the task-sized boundary:

10. **Run identity** is no longer the MCP connection. Explicit `begin_wiki_run` /
    `end_wiki_run` with a label define a task run; when none is open, each
    mutating MCP call is its own implicit run. The manifest and
    `run_row_changes` gain `label`, `kind` and `closed_by`; `record_wiki_run`
    becomes a compatibility close; instructions bump to version 2.
11. **The open run is a database row.** `run_undo.last_heartbeat_at` plus a
    unique partial index on `state = 'open'` make one-open-run a database
    guarantee. A second MCP client is refused; a heartbeat expiry of 10 minutes
    makes a dead run reclaimable, and the 24-hour sweep reuses that field.
12. **Directory-shaped deletes share one capture helper.** `notes::delete_path`,
    `documents::delete_path` and both project deletes call `capture_tree` before
    removal, so files, empty directories and symlinks all come back.

---

## Unverified / to confirm during implementation

- `sources::hash_bytes` algorithm and cost on large PDFs.
- `documents::write_text`, `create_markdown`, and `import_file` internals.
- Whether any non-app path writes into project folders besides the confirmed
  `import_file` upload (external tools are expected).
- Exact `db::migrations` registration mechanics and the version constant.
- Whether `VACUUM INTO` through `rusqlite` sees the latest committed WAL state on
  the app's and MCP's connections (expected, but test with a two-connection
  setup).
- Whether `PRAGMA data_version` reliably detects a foreign commit in the app/MCP
  process model; the contention flag depends on it.
- Whether the MCP server's shutdown path can run `end_wiki_run` on SIGTERM/SIGINT,
  and how quickly the OS tears the process down; abandoned runs assume it cannot
  always complete.
- Row-hash canonicalization must be stable across TEXT, INTEGER, REAL and NULL
  values; test with representative rows.
- Whether `foreign_keys=OFF` plus per-key delete/reinsert handles the
  `clock_people → clocks` RESTRICT relationship and all unique indexes.
- Snapshot disk-space failure behaviour (`VACUUM INTO` errors when the history
  volume is full).
- Whether SQLite enforces the partial unique index on `run_undo(state)` across
  the app and MCP processes under WAL exactly as intended (it should; test a
  second `open` insert while the first is open).
- Whether the MCP server can refresh `last_heartbeat_at` between long tool calls
  without a timer, and whether a 10-minute expiry fits real agent think time;
  `heartbeat_wiki_run` is the escape hatch for time spent outside MCP.
- Whether `fs::read_dir` enumeration can race with a concurrent delete so that a
  vanished entry is recorded as missing rather than aborting the whole capture.
- APFS `clonefile` availability under the app bundle and MCP server.
- Whether the current three-half Knowledge work removed the `KANDRIUM_NOTES`
  alias.
