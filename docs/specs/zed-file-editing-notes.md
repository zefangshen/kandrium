# Zed file editing — architecture notes and Kandrium applicability

Read-only research. No Kandrium code changed.

## Revision read

- Intended source: `/Users/zefang/Documents/resources/zed`.
- That checkout is **not usable**: it contains only a `.git` skeleton (HEAD
  points at `refs/heads/.invalid`, no refs, no working tree, one 80 KB aborted
  temporary pack). There is no local revision to report.
- Actual revision read: upstream `zed-industries/zed` at commit
  **`985bbf42e684ef5b5a9312a6abf075aeafbaf5b9`** (`refs/heads/main` at the time
  of reading), fetched file-by-file over HTTPS. Line references below are
  against that commit.
- Everything here is architecture-level description. No Zed code, comments,
  identifiers, doc prose or fixtures were copied; file names and public type
  names are cited only for navigation.

## Licence map (for every crate cited)

| Crate | Licence |
| --- | --- |
| `gpui` | Apache-2.0 |
| `gpui_macros` | Apache-2.0 |
| `editor` | GPL-3.0-or-later |
| `text` | GPL-3.0-or-later |
| `rope` | GPL-3.0-or-later |
| `language` | GPL-3.0-or-later |
| `multi_buffer` | GPL-3.0-or-later |
| `project` | GPL-3.0-or-later |
| `worktree` | GPL-3.0-or-later |
| `fs` | GPL-3.0-or-later |
| `vim` | GPL-3.0-or-later |
| `ui` | GPL-3.0-or-later |

Kandrium is Apache-2.0. Ideas are read for architecture only; no GPL crate may
be vendored or added as a dependency, and no GPL file may be transcribed or
paraphrased line-by-line. Where an idea collapses into copying a data structure
or algorithm from a GPL file, it is marked **do-not-copy** below.

## 1. Buffer and text model

**Representation.** A file does not belong to an editor. The project owns a
buffer store that maps a project path to one shared buffer entity
(`crates/project/src/buffer_store.rs`). An editor holds a multi-buffer that
references those shared buffers (`crates/multi_buffer`), which is why several
panes, previews and the vim layer can show the same file without duplicating
text.

**Text storage.** The buffer wraps `text::Buffer`
(`crates/text/src/text.rs`), a versioned text store: immutable versions,
fragments, insert/delete operations with Lamport clocks and per-replica
bookkeeping, plus an undo map and an operation queue. The rope
(`crates/rope`) is the flat character storage and point/offset conversion
layer. The clock/replica machinery exists for collaborative editing — it is
not needed for a local single-writer app.

**Applying edits.** An edit is a set of range replacements applied as one
transaction, producing a new version. Subscribers receive a patch describing
the change (`text/src/patch.rs`), which is how anchors, diagnostics, syntax
trees and other consumers stay current without rescanning the text.

**Undo/redo.** The text buffer owns history and undo grouping; the editor calls
into the buffer rather than keeping its own stack. Consequence: every view of
the same buffer shares one undo history. Grouping is transaction-based with a
time-based coalescing interval in the buffer.

**Selections/carets.** Editor selections are stored as `Selection<Anchor>` in a
selections collection (`crates/editor/src/selections_collection.rs`,
`crates/text/src/selection.rs`). An anchor is a position that a patch can
resolve against a later version, so edits do not require hand-fixing every
caret. Screen positions are resolved through the editor's display map, which
also carries soft wrap and folds.

## 2. Open → edit → save

**Open.** The project resolves a path through the worktree and asks the buffer
store for a buffer; if the path is already open, the existing buffer entity is
returned. Language parsing, diagnostics and highlighting attach to the buffer,
not the view. Opening the same file twice therefore yields two views over one
buffer (one dirty flag, one history).

**Dirty.** A buffer is dirty when the text version differs from the version at
the last save/load; the result is memoised against the version so the check is
cheap. Disk state (mtime + size) is tracked alongside it, which is what makes
conflict detection possible.

**Save.** The store snapshots the text, writes it through the worktree/fs
layer, then records the saved version and the disk mtime. Notably, Zed's
normal buffer save is a truncate-then-write (`crates/fs/src/fs.rs`); the
atomic temp-file-and-rename helper in that crate is used for settings, not for
buffer saves. **Kandrium's `fsutil::write_atomic` is already stronger than the
path Zed uses for documents.** There are no editor-managed backups.

**Multiple views.** The buffer store is the dedupe point. Editors, multi-buffer
excerpts and the vim layer are consumers of one buffer entity, so text, dirty
state, undo history and syntax parse are shared.

## 3. External change detection

- The worktree watches the filesystem (`crates/worktree`, `crates/fs`, via an
  OS watcher) and updates each file's recorded disk state (mtime and size).
- The buffer store compares that state with the buffer's saved mtime. When the
  disk file is newer and the buffer has **no unsaved edits**, it signals that a
  reload is needed; when the buffer is dirty it keeps the buffer and records a
  conflict.
- Reload is not a blind reload: the language buffer diffs disk text against the
  buffer's base version and applies the difference as an ordinary, undoable
  transaction, preserving anchors. If the buffer version moved during that
  work, it declines and marks the conflict instead of clobbering.
- A dirty→clean transition (undoing back to the saved text) re-checks the disk
  state, because that is the first moment a silent reload is safe again.
- Saving a buffer whose disk mtime no longer matches is treated as a conflict;
  the user decides whether to overwrite or reload.

So: clean buffers auto-reload silently and undoably; dirty buffers always keep
the user's text and surface the conflict rather than resolving it silently.

## 4. Syntax and highlighting

- Tree-sitter, per language: a grammar plus query files configured per language
  (`crates/language/src/language.rs`, `languages/<lang>/config.toml`), covering
  highlights, injections, brackets, outline and similar.
- Parsing is incremental: edits produce a tree-sitter input edit, the buffer
  reparses off the UI thread against its last tree, and the result is stored in
  a layered syntax map (`crates/language/src/syntax_map.rs`) that also handles
  injected languages inside a host document.
- Highlight ranges are cached per display chunk and composed with semantic and
  diagnostic ranges by the editor's display map. The parse cache belongs to the
  buffer, so all views of a file share it.
- Minimum viable form of the idea: one grammar for one language (Markdown),
  incremental reparse on edit, a static mapping from capture names to theme
  colours, and no injected languages, no outline, no brackets, no LSP.

Caveat for Kandrium: in gpui-kit 0.9.1, highlighting is exposed on the
code-editor state, not the plain textarea state. `TextareaState`
(`InputBaseState<TextareaMode>` in `gpui-base`) is always plain-text layout;
`set_highlighter_factory` and the `InputHighlighter` trait live on
`EditorState` (`InputBaseState<EditorMode>`). A language and syntax provider are
application-supplied through `LanguageConfig`/`SyntaxContextProvider`. So
"add Markdown highlighting" in the current editors is really "replace
`TextareaState` with `EditorState`", which also interacts with the vim layer
and is not a small change. (gpui-base is Apache-2.0, so the dependency itself
is not a licence problem.)

## 5. Pattern vs dependency

**Implementable in plain Rust (no GPUI internals):**

- One shared buffer/session object per path, owned by the data layer, with
  text, saved text, dirty flag, disk identity and load/save/reload decisions.
- Save through an atomic writer; dirty = current text vs last saved text.
- Disk-state tracking (mtime + size) and a clean-vs-dirty decision on external
  change; conflict instead of silent overwrite.
- Undo owned by whatever object owns the text, so all views of that object
  share it.
- Per-language configuration data and an incremental parser behind a small
  trait.
- Anchor-style positions (a position plus a change log) as an idea; a much
  simpler offset-adjustment scheme is enough for a single editor.

**Only viable because of Zed's architecture:**

- The versioned CRDT text store with clocks/replicas and the undo map: exists
  for collaboration and remote buffers. Copying it would be both GPL-tainted
  and pointless locally.
- `multi_buffer` excerpts and the editor display map: these are the mechanism
  by which one pane can hold many buffers and map anchors through folds/wrap.
- Buffer-owned undo *as implemented there*: it depends on the text store's
  version history.
- Buffer-store + worktree + language-registry wiring as separate background
  entities: it assumes GPUI's entity/context/executor model and a workspace
  bigger than Kandrium's.
- Tree-sitter highlighting pipeline (syntax map, injection layers, chunk
  caches): tied to the editor's display map.

## Applicability to Kandrium

Current state (for context): the gpui app edits Markdown in `TextareaState`
with a vim layer outside it, per-tab state, and a 400 ms debounced autosave
through `kandrium_core` (`fsutil::write_atomic`). Notes, Documents and project
documents each keep their own tab list and their own autosave path, but all go
through core `notes`/`documents`/`projects` modules. The gpui app's background
watcher polls the database only; file-backed sections are deliberately skipped
in external-change handling, so an open note or document never notices an
on-disk edit and the next autosave overwrites it.

| Idea | Verdict | Reason / cost / risk |
| --- | --- | --- |
| One shared document session per file, owned by `kandrium-core`, used by Notes, Documents and project docs | **adopt-in-simplified-form** | Removes three copies of tab/autosave logic and gives one dirty flag per path. Cost: one core module plus three view refactors (medium). Risk: low; APIs stay synchronous like the rest of core. No CRDT, no version graph — just text + saved text + disk identity. |
| Disk-state tracking and conflict-aware save (mtime+size, never silently overwrite a changed file) | **adopt-in-simplified-form** | Core records mtime/size on load and save; save refuses or asks when the file changed underneath. Cost: small API addition plus a confirm path in the views (small–medium). Risk: moderate around clock resolution and our own atomic rename changing mtime; needs tests. |
| Clean-buffer auto-reload on external change; dirty-buffer banner with Reload / Keep mine | **adopt-in-simplified-form** | Poll on section focus or on the existing watcher tick instead of an OS watcher; clean reload replaces the editor text, dirty keeps it and shows a banner. Cost: small. Risk: caret loss on reload (acceptable for clean buffers) and races with debounced saves; a save must re-check mtime first. |
| Buffer-owned undo shared by all views of a file | **do-not-copy** | Would require owning the text model; `TextareaState` already owns undo per editor. The shared-session idea above gets most of the benefit by ensuring one tab/editor per path. |
| Anchor-based selections/carets | **do-not-copy** | Depends on the versioned text store; `TextareaState` already keeps carets stable for its own edits. |
| Multi-buffer / excerpts | **do-not-copy** | Kandrium opens whole files in tabs; there is no use for excerpts, and the machinery is GPL and GPUI-bound. |
| Debounced autosave into a plain file | **keep, do not replace with Zed's explicit save** | Kandrium already writes atomically; Zed's buffer save is truncate-in-place. Explicit save would add prompts and losing-work risk with no benefit for a local notes app. Keep the 400 ms debounce and add conflict checks around it. |
| Tree-sitter Markdown highlighting | **do-not-copy for now; adopt-in-simplified-form only if the editors move to `EditorState`** | The current `TextareaState` cannot highlight; the change is a component swap plus a grammar/query and a highlighter implementation, and it touches the vim layer. Cost: high and uncertain. Risk: high. A regex-based highlighter would not fit the current component either. |
| Per-language config files, injection layers, outline/brackets from the same registry | **do-not-copy** | Kandrium needs at most Markdown; a whole language registry is unjustified. |
| Background parse worker and syntax map | **do-not-copy** | Only pays off with a real editor surface and large files. |

## What is most worth doing

1. **A core `DocumentSession`** (adopt-in-simplified-form): path + current text
   + last-saved text + dirty + last-known mtime/size + `load` / `save` /
   `check_disk` decisions, shared by Notes, Documents and project documents.
   This is the honest, small version of "one buffer shared by many views" and
   removes the current three-way duplication.
2. **Conflict-aware save and external-change reconciliation**
   (adopt-in-simplified-form): never overwrite a file whose mtime/size changed
   since load without asking; silently reload a clean buffer; keep a dirty one
   and offer Reload / Keep mine. This is the largest real-world data-loss gap
   in the current app, and it needs no OS watcher or GPUI machinery.
3. **Keep the debounced atomic autosave** (do-not-change): it is simpler and
   safer than Zed's explicit-save path. Do not adopt buffer-owned undo, the
   CRDT text store, multi-buffer, or Zed's non-atomic buffer save. Treat
   Markdown syntax highlighting as a separate, optional project gated on
   moving from `TextareaState` to `EditorState`; do not attempt it as a small
   add-on.
