# Plan: an LLM wiki over Documents and Wiki pages

Status: proposed. Scope: `crates/core`, `crates/mcp`, `crates/app-gpui`,
`migrations/`, and this spec folder. Read-only research is complete; no
production code has been changed.

## 1. Fixed decisions

These are settled and are not reopened by this plan:

- **Documents are the raw layer.** `KANDRIUM_DOCS` (default
  `<data_dir>/kandrium/documents/`) holds PDFs and Markdown. The wiki never
  moves, rewrites, or deletes a source file.
- **`wiki/` is the page root.** `KANDRIUM_WIKI` (default
  `<data_dir>/kandrium/wiki/`) holds the knowledge base. There is no separate
  notes root.
- **Compiled output lives under `wiki/`.** Generated pages use
  `wiki/{sources,entities,concepts}` plus `wiki/index.md`; anything
  hand-written lives elsewhere under the same root. No new top-level section
  is introduced.
- **Kandrium stays Apache-2.0.** GD4AI/obsidian-llm-wiki (Apache-2.0) is the
  primary design reference; lucasastorian/llmwiki (Apache-2.0) is the secondary
  reference for the workspace/index seam. GPL/AGPL projects are read for ideas
  only.
- **The deterministic core is the product.** Hashing, dedup detection, merge
  detection, index rebuild, lint, link resolution, path guarding, and atomic
  writes always belong to `kandrium-core`.
- **No in-app model client.** The external-agent path was chosen; Kandrium
  does not implement providers, keys, retries, token budgets, or prompts.

## 2. Decision: no LLM client in Kandrium

**Decided: Shape B, the external-agent path.** Kandrium ships no model client,
no provider layer, and no API-key handling. The agent reads the documents,
extracts knowledge under the instructions in §6.4, and writes pages through
MCP. Kandrium owns only the deterministic contract around those writes.

### 2.1 The two shapes

**Shape A — in-app provider layer.** Kandrium owns prompt templates, model
selection, retries, token budgets, key storage, and a compile button.

**Shape B — external agent over MCP.** The user runs Codex, Claude, or another
MCP client. The agent reads sources and writes pages through the tools in §3.
Kandrium owns the file contract, validation, index, lint, and run log.

### 2.2 Honest trade-offs

| Axis | In-app provider (A) | External agent over MCP (B) |
|---|---|---|
| **Determinism** | Prompts and pipeline version are fixed by Kandrium, but LLM output is still non-deterministic. | LLM output is equally non-deterministic. Determinism comes from Kandrium's schema, validation, and stable file layout in both shapes. |
| **Cost control** | Kandrium can enforce per-run token and page caps, but must expose spend. Keys and billing are the app's problem. | The user's existing subscription pays; Kandrium cannot meter or cap it. A runaway agent costs the user, not the app. |
| **Offline use** | Possible only if the user configures a local endpoint (Ollama/LM Studio) and Kandrium implements it. | Possible with a local-model agent, but not with a cloud-only agent. Neither shape is offline by default. |
| **Who debugs a bad page** | Kandrium owns the prompt and the run, so the user comes to Kandrium. That means prompt debugging, model drift, and provider-specific failures are Kandrium bugs. | The agent transcript is the debugging surface. Kandrium only says "this page violates the contract"; it does not own the generation. |
| **When the agent/model is unavailable** | A missing key, expired token, or offline network leaves a dead button. | Nothing in the knowledge base breaks. Sources stay indexed; pages stay readable, searchable, and lintable. The UI shows "uncompiled sources" and setup instructions. |
| **Maintenance surface** | Every provider, streaming mode, retry policy, context limit, and model rename is permanent work. | MCP is one stable seam. The agent vendors own model access, auth, and tool use. |
| **Privacy** | The app decides where source text goes. Users must trust the app's provider choice. | The user explicitly configures the agent and its provider. Read-only vs write scope can be granted per client. |
| **UX** | One click. Best for a non-technical user. | Setup requires an MCP client entry and a prompt. Best for the user who already runs Codex/Claude. |
| **Failure visibility** | Kandrium can show a precise stage and retry. | The agent reports its own failures; Kandrium sees only what reached the write tools. `record_wiki_run` closes part of that gap. |

Shape A was considered and rejected for this design. It is documented only to
explain the boundary; it is not a planned fallback.

### 2.3 The boundary

Adopt **Shape B** with a narrow boundary:

- **Kandrium owns:** source scanning and hashing, source state, dedup/merge
  candidates, page schema validation, path guards, atomic writes, wikilink
  resolution, the SQLite index and FTS5, lint, the run ledger, and the UI.
- **The agent owns:** reading sources, extracting entities and concepts,
  deciding merges, writing page bodies and frontmatter, and recording what it
  did.
- **Kandrium never sends source text to a network.** The agent does that under
  the user's own account and configuration.
- **The wiki remains useful with no agent.** A source that was never compiled
  is still listed, readable, and lint-flagged. This is strictly better than a
  dead in-app button with a missing key.
- **The UI does not call an LLM.** "Compile" opens a setup panel that copies
  the MCP client entry and points at `get_wiki_instructions`. It is
  orchestration guidance, not a provider.

### 2.4 What this decision does not change

- It does not mean no model-facing code. Kandrium still validates frontmatter,
  rejects malformed pages, and gives the agent deterministic merge candidates.
- It does not mean the agent can write anywhere. It means the agent writes
  only under the `wiki/` root, through guarded tools, with optimistic concurrency.
- It does not move generation inside the app. There is no in-app provider to
  add later; the agent contract is the only generation path.

## 3. Question 2: can an MCP server expose the wiki?

Yes. The existing `crates/mcp` server already has the transport, bootstrap,
logging, and result conventions. Extend it; do not add a second binary in v1.

### 3.1 Extend `crates/mcp`, do not add `kandrium-wiki-mcp`

Reasons:

- One MCP client entry exposes boards and the wiki. A second binary means a
  second config entry, a second DB bootstrap, and a second logging component.
- The DB and the wiki index are one file; the existing server already opens it
  with the right pragmas.
- The tools are additive and namespaced. The existing 13 Kanban tools keep
  their names, schemas, and result shapes.
- A separate binary is justified only if a client must be denied Kanban tools
  or the wiki must run with a different trust boundary. The `KANDRIUM_MCP_WIKI`
  gate in §3.2 covers the common case without a new executable.

Cost: the advertised tool list grows from 13 to 26 (12 wiki read/write tools
plus `get_wiki_instructions`). Clients tolerate extra tools; the existing
router test must be updated to assert both sets.

### 3.2 Naming and gating

- Tool names are verb-first, matching `list_boards`, `get_board`, and
  `create_card`: `list_wiki_sources`, `read_wiki_source`, `list_wiki_pages`,
  `read_wiki_page`, `search_wiki`, `get_wiki_status`, `get_wiki_instructions`,
  `lint_wiki`, `write_wiki_page`, `patch_wiki_page`, `delete_wiki_page`,
  `record_wiki_run`, `list_wiki_dedup_candidates`.
- Inputs use `#[serde(rename_all = "camelCase")]`; outputs are pretty-printed
  JSON text; failures set tool-level `isError` with a message, exactly like
  `KandriumServer::run`.
- `KANDRIUM_MCP_WIKI=off|read|write` gates the handlers, default `write`.
  `off` returns an error for every wiki tool; `read` rejects the four write
  tools. The tools stay advertised, because stdio is local and the gate is a
  guardrail, not a security boundary.
- Every write commits a `wiki_runs` row. That is also what makes the gpui app
  see an external change through the existing `PRAGMA data_version` watcher.

### 3.3 Tools and schemas

Read side:

| Tool | Input | Output |
|---|---|---|
| `list_wiki_sources` | `{ pathPrefix?: string, state?: "new"\|"indexed"\|"compiled"\|"stale"\|"failed", limit?: number }` | `{ sources: [{ path, kind, state, hash, size, modifiedAt, compiledAt, pageCount }], counts: { new, indexed, compiled, stale, failed } }` |
| `read_wiki_source` | `{ path: string, startLine?: number, endLine?: number, startPage?: number, endPage?: number, maxPages?: number }` | `{ path, kind, text, startLine, endLine, pageCount, truncated, nextPage? }` |
| `list_wiki_pages` | `{ kind?: "source"\|"entity"\|"concept"\|"index"\|"page", source?: string, limit?: number, offset?: number }` | `{ pages: [{ path, kind, title, summary, aliases, tags, reviewed, updatedAt }], total }` |
| `read_wiki_page` | `{ path: string }` | `{ path, frontmatter, body, contentHash, reviewed, sources: [{ path, startLine, endLine, startPage, endPage }], backlinks: [{ from, anchor }] }` |
| `search_wiki` | `{ query: string, scope?: "pages"\|"sources", limit?: number }` | `{ hits: [{ path, kind, title, summary, score, snippet }] }` |
| `get_wiki_status` | `{}` | `{ sourceCounts, pageCounts, lastRun, indexRevision, wikiRoot, documentsRoot, instructionsVersion }` |
| `get_wiki_instructions` | `{}` | `{ version, schemaVersion, instructions, instructionsHash, tagVocabulary }` |
| `lint_wiki` | `{ path?: string, fix?: boolean }` | `{ findings: [{ code, severity, path, line, message, fixable }], fixed: [{ code, path }] }` |

Write side:

| Tool | Input | Output |
|---|---|---|
| `write_wiki_page` | `{ path, content, expectedHash?, force?, agent?, instructionsVersion }` | `{ path, previousHash, contentHash, action: "created"\|"replaced"\|"unchanged" }` |
| `patch_wiki_page` | `{ path, oldText, newText, expectedHash?, force?, agent?, instructionsVersion }` | `{ path, replaced: number, contentHash }` |
| `delete_wiki_page` | `{ path, expectedHash, force?, agent?, instructionsVersion }` | `{ path, deleted: true }` |
| `record_wiki_run` | `{ operation: "ingest"\|"merge"\|"lint"\|"repair"\|"maintenance"\|"edit"\|"write", agent, instructionsVersion, startedAt, finishedAt?, sources?, pagesCreated?, pagesUpdated?, pagesSkipped?, note? }` | `{ id, recorded: true }` |
| `list_wiki_dedup_candidates` | `{ source: string, limit?: number }` | `{ candidates: [{ existingPath, score, reason }] }` |

Schema rules:

- `path` for pages is relative to the wiki root. Compiled pages use
  `(sources|entities|concepts)/<slug>.md`; hand-written pages may use any
  other `*.md` path under the root. `index.md` is generated-only and rejected
  by `write_wiki_page`.
- `path` for sources is relative to the documents root, `/`-separated, never
  absolute, never containing `..`.
- `slug` is lowercase `a-z 0-9 -`, 1–80 chars, no leading or trailing hyphen.
- `content` must start with a `---` frontmatter block containing at least
  `type` and `title`. `sources` is required for `type: source`.
- MCP writes always require frontmatter. Plain hand-written pages are an app
  affordance; an agent creating one goes through the same schema.
- Every write tool requires `instructionsVersion`. A mismatch is rejected with
  "instructions stale: call get_wiki_instructions"; the agent must re-read the
  instructions and retry. This is the only way to keep an old prompt from
  writing against a new schema.
- PDF reads default to 20 pages per call (`maxPages` capped at 50) and report
  `truncated`/`nextPage`; Markdown reads default to 2,000 lines.
- `search_wiki` scope `pages` uses FTS5. Scope `sources` is an on-demand scan
  of Markdown plus PDF text extraction, capped and explicitly labeled; it is
  not a persisted source index in v1.

Concurrency and protection:

- `write_wiki_page` creates when the path is missing. Replacing requires
  `expectedHash` unless `force: true`.
- A page with `reviewed: true` (set automatically when the user saves it in
  the app) is rejected unless `force: true`. A forced overwrite is recorded in
  `wiki_runs.note`.
- `patch_wiki_page` requires `oldText` to occur exactly once; `expectedHash`
  is recommended and enforced when supplied.
- `delete_wiki_page` requires `expectedHash` and rejects `reviewed` pages
  unless `force: true`.
- One writer per page is assumed. SQLite uses `BEGIN IMMEDIATE` for the index
  update; file writes are atomic per file, so the worst case is a lost update
  that lint reports as `index-drift`, repaired by `rebuild`.

### 3.4 What an MCP client can and cannot do

Can:

- Read every source and page, search pages, inspect compile state and lint
  findings.
- Create, replace, and patch pages under the `wiki/` root, with optimistic
  concurrency.
- Record a run, so the app refreshes and the history is auditable.

Cannot:

- Write, rename, or delete anything under the documents root.
- Escape the wiki root or write outside it (`fsutil::guard` plus a root-prefix
  check).
- Overwrite a `reviewed` page without an explicit `force`.
- Run shell commands, call a model itself, render UI, change settings, or
  start background work that outlives the connection.
- Guarantee a multi-file transaction. Each file is atomic; the index is
  eventually consistent and rebuildable.
- Read PDF annotations or edit sidecars. Those remain app-only.

Compared with an in-app agent, the MCP client has the same file-write power
but no UI, no automatic retry, and no visibility into prompts or token spend.
Kandrium's job is to make the file contract strict enough that this is safe.

### 3.5 Constraints carried from the existing server

`AGENTS.md` and `crates/mcp/src/main.rs` impose:

- stdio transport; stdout is reserved for MCP, so no `println!` anywhere.
- The existing 13 tool names and schemas must not change. Adding tools is
  allowed; renaming or removing one is not.
- camelCase JSON, pretty-printed text results, tool-level `isError`.
- `core::logging` for diagnostics, `core::bootstrap` for DB startup.
- Tests must assert the router contents; the current
  `tool_router_exposes_the_curated_set` test becomes
  "the original 13 are still present, and the wiki tools are present".

Implementation shape: `crates/mcp/src/wiki.rs` holds the wiki input structs,
handlers, and tests; `main.rs` keeps the single `#[tool_router]` impl and adds
thin `#[tool]` wrappers that call `wiki::...`. `KandriumServer` gains
`wiki_root: PathBuf` and `documents_root: PathBuf`, resolved once at startup
from `core::wiki::resolve_wiki_root()` and
`core::documents::resolve_documents_root()` and injected with temp roots in
tests. A `run_wiki` helper mirrors `run` but passes both roots to core.

## 4. What exists today

Read before designing:

| Area | Fact |
|---|---|
| `crates/core/src/lib.rs` | Flat modules: `clocks`, `documents`, `notes`, `projects`, `db`, `services`. Exports are re-exported here. |
| `crates/core/src/notes.rs` | The current page-tree layer: `resolve_notes_root`, `ensure_root`, `scan_tree`, `read_note`, `write_note`, `create_note`, `create_folder`, `rename_path`, `delete_path`. All take `&Path root`; writes go through `fsutil::write_atomic`. The plan retargets this root to `wiki/` and renames the module. |
| `crates/core/src/documents.rs` | Same shape for PDF/Markdown; `DOC_EXTENSIONS = ["pdf","md","markdown"]`; annotations are JSON sidecars. `read_text` only reads Markdown. |
| `crates/core/src/fsutil.rs` | `scan_tree`, `guard` (root confinement), `write_atomic`, `create_new_file`, `has_extension`. Reuse all of it. |
| `crates/core/src/db/migrations.rs` | `SCHEMA_VERSION = 11`; embedded numbered SQL; `Migrations` list; `bootstrap` snapshots before pending migrations. |
| `crates/core/src/db/connection.rs` | WAL, `foreign_keys=ON`, 5 s busy timeout, read-only variant. |
| `crates/core/src/error.rs` | `KandriumError(String)`; no structured error enum. Keep messages user-readable. |
| `crates/mcp/src/main.rs` | One `#[tool_router] impl KandriumServer`; `run()` locks a `Mutex<Connection>`; 13 tools; router test. |
| `crates/mcp/Cargo.toml` | `rmcp 3.5`, tokio, serde, rusqlite; no file-watching dependency. |
| `crates/app-gpui/src/views/wiki.rs` | Today `WikiState { notes, documents, order, active, split, notes_collapsed, docs_collapsed }`; the plan renames these halves to `wiki` (page root) and `sources` (documents root). `TabKind::{Note,Doc}`; one shared tab row. |
| `crates/app-gpui/src/app.rs` | `wants_watcher()` excludes `Section::Wiki`; `apply_external_change` returns early for Wiki; today's `load_notes()` and `load_documents()` become `load_wiki_pages()` and `load_sources()`. |
| `crates/pdf-render/src/lib.rs` | `PageText`, `TextLine`, `TextGlyph`, `group_lines`; worker owns the PDF. No synchronous file-to-text helper yet. |
| `Cargo.toml` | Workspace excludes `crates/app-gpui`; core deps do not include `sha2`; bundled SQLite enables FTS5 (verified in `libsqlite3-sys-0.38.2/build.rs`). |

Gaps the plan must fill:

- No frontmatter parser or writer anywhere.
- No SQLite index or FTS5 table.
- No link graph, backlinks, or lint.
- No LLM/provider code, and no MCP file tools.
- The app does not refresh when an external process writes wiki pages or documents.

## 5. File-truth model

```
<documents root>/                     raw, immutable
  papers/attention.pdf                ──► wiki_sources
  readings/reading.md

<wiki root>/                          pages, compiled + hand-written
  index.md                            generated table of contents
  sources/attention-is-all-you-need.md
  entities/vaswani.md
  concepts/self-attention.md
  handwritten/my-own-page.md          user-owned, never touched
```

Rules:

1. **Files are canonical; SQLite is derived.** Deleting the wiki tables and
   running rebuild must reproduce the index from the files.
2. **Everything under `wiki/` is managed.** Compiled pages live in
   `sources/`, `entities/`, and `concepts/`; hand-written pages may live
   elsewhere under the root. A user may read and edit any page; the app marks
   it reviewed on save. A file without frontmatter inside the three compiled
   folders is reported as `unmanaged-page`, not deleted; hand-written pages
   elsewhere may be plain Markdown and are indexed as kind `page`.
3. **Nothing outside `wiki/` is written by the wiki.** Hand-written pages are
   never rewritten, moved, or renamed by compile, lint, or MCP.
4. **Documents are read-only for the wiki.** Only the existing upload/create
   paths may change them.
5. **All stored paths are root-relative and `/`-separated.** No absolute paths
   in frontmatter or the DB; this keeps the wiki root portable.
6. **Write order is file first, index second.** `fsutil::write_atomic` commits
   the file; the DB row is updated immediately after. A crash in between
   leaves a file that `rebuild` or `refresh` will index.
7. **`index.md` is generated.** Agents may not write it; `rebuild` and page
   writes regenerate it from `wiki_pages`.

## 6. Page model and provenance

### 6.1 Frontmatter

A deliberately small YAML subset (scalars and string arrays), hand-parsed in
Rust to avoid a YAML dependency:

```yaml
---
type: source | entity | concept
title: Multi-Head Attention
summary: One-line definition.
created: 2026-11-13
updated: 2026-11-13
sources: [papers/attention.pdf, readings/reading.md]
tags: [machine-learning]
aliases: [MHA, multi-head self-attention]
reviewed: true
contradicted_by: [concepts/positional-encoding.md]
---
```

- Compiled pages require `type` and `title`. Hand-written pages elsewhere under
  the root may omit frontmatter; they are indexed as kind `page`, with the
  first `# heading` or the filename as title.
- `sources` is required and non-empty for `type: source`; optional but
  recommended otherwise.
- `aliases` is recommended; lint warns when empty.
- Unknown keys are preserved verbatim on read and written back unchanged.
- `reviewed` is the user-edit contract. The app sets it on save; MCP writes
  cannot clear it.

### 6.2 Provenance

Two layers, one human-readable and one queryable:

- Frontmatter `sources` lists document-relative paths.
- Inline footnotes anchor claims:

```markdown
Self-attention relates each token to every other token in the sequence.[^1]

[^1]: documents/papers/attention.pdf#page=3
```

For Markdown sources the anchor is `#L12-L18` (1-based, inclusive). For PDFs
it is `#page=N` (1-based). A citation range is optional; a whole source is
valid.

The DB mirrors the same data in `wiki_page_sources` so lint and backlinks do
not re-parse every page. A citation is valid only if its source path exists in
`wiki_sources`; otherwise `bad-citation`.

### 6.3 `index.md`

Generated after every page write or rebuild:

```markdown
# Wiki index

## Concepts
- [[concepts/self-attention|Self-Attention]] `aliases: self-attention` `source: attention-is-all-you-need` - Relates each token to every other token.

## Entities
- [[entities/vaswani|Ashish Vaswani]] `aliases: vaswani` `source: attention-is-all-you-need` - Lead author of "Attention Is All You Need".

## Sources
- [[sources/attention-is-all-you-need|Attention Is All You Need]] `source: papers/attention.pdf` - 2017 Transformer paper.
```

The line format is deliberate: a human can read it, and a tiny parser can
recover path, alias list, source slug, and summary.

### 6.4 Agent instructions

The agent's behaviour is the product now, so the instructions are a versioned
contract, not a prompt someone edits once. They live in one canonical
Markdown file, are served by MCP, and are enforced wherever a rule can be
checked mechanically.

What the agent is told:

1. **Start the session by calling `get_wiki_instructions`.** Use the returned
   `version` on every write. If a write returns "instructions stale", fetch the
   new version, re-read the changed rules, and retry.
2. **Page schema and frontmatter.** Compiled pages go in
   `sources/<slug>.md`, `entities/<slug>.md`, or `concepts/<slug>.md`;
   hand-written pages go elsewhere under `wiki/`. Every page starts with:
   - `type`: `source`, `entity`, or `concept`.
   - `title`: the canonical human title.
   - `summary`: one sentence, no Markdown.
   - `sources`: document-relative paths; required for `type: source`.
   - `aliases`: at least one alias, including abbreviations and other
     languages.
   - `tags`: only from the vocabulary returned with the instructions.
   - `created` / `updated`: ISO dates.
   - `reviewed`: never set or cleared by the agent; it is the user's edit flag.
   - `contradicted_by`: optional page paths when a claim conflicts.
   Body shape: a one-paragraph definition, then cited claims, then a
   `## Related` section of wikilinks. No title repetition, no frontmatter-like
   text in the body.
3. **Citations.** Every non-trivial claim carries a footnote:
   - Markdown source: `[^1]: documents/readings/reading.md#L12-L18`
     (1-based, inclusive).
   - PDF source: `[^1]: documents/papers/attention.pdf#page=3`
     (1-based).
   - Cite only lines/pages actually read through `read_wiki_source`; never
     invent a range; do not cite a source that is not in `list_wiki_sources`.
   - Reuse one footnote for repeated claims. If a PDF has no text layer,
     record the source as unreadable instead of guessing.
4. **Merge before create.** Search pages by title, alias, and source first.
   - Create only when no page has the same normalized title or alias.
   - Merge when the entity/concept is the same, even from another source:
     add the source to `sources`, append the new cited claims, keep existing
     aliases, and add the new title as an alias if it differs.
   - One source page per source; entity and concept pages are shared across
     sources. Never create a page per source for an entity or concept.
   - On a real contradiction, keep both claims with their citations and set
     `contradicted_by`; never silently replace the older claim.
   - Merge into the canonical page rather than deleting the duplicate. Delete
     only with `delete_wiki_page`, a matching `expectedHash`, and a recorded
     run.
5. **Links.** Use `[[concepts/self-attention|Self-Attention]]` relative to the
   wiki root. Link the first prose mention of another page. Never link inside
   code, existing link text, image alt text, or table cells; no self-links; no
   dead links. `index.md` is generated and must not be written.
6. **Must not.** Write outside `wiki/`; touch documents; write `index.md`; set
   or clear `reviewed`; invent citations; use absolute paths or `..`; add
   frontmatter keys outside the schema; use `force` without a user request;
   delete a page without a recorded run.
7. **Finish with `record_wiki_run`.** Report the operation, sources read,
   pages created/updated/skipped, the instruction version, and any sources
   that failed extraction.

Enforcement is split honestly:

- **Hard checks** in the write tools: root confinement, `*.md` only,
  `index.md` rejection, frontmatter parse and required keys, slug rules,
  `instructionsVersion`, `expectedHash`, `reviewed` protection, and
  `force` recording.
- **Soft checks** in lint: merge quality, orphan/dead links, citation
  validity, contradictions, stale pages. The agent can still write a weak
  page; lint makes it visible and reviewable rather than invisible.

### 6.5 Instruction versioning and delivery

- **One source.** `crates/core/src/wiki/instructions.md`, embedded with
  `include_str!` by `crates/core/src/wiki/instructions.rs`, next to
  `pub const WIKI_INSTRUCTIONS_VERSION: u32`. There is no second copy in
  settings, a database row, or the Setup panel.
- **One delivery.** `get_wiki_instructions` returns
  `{ version, schemaVersion, instructions, instructionsHash, tagVocabulary }`.
  Tool descriptions tell the agent to call it first; `get_wiki_status` also
  reports the current version.
- **One gate.** Every write tool requires `instructionsVersion`. A mismatch
  returns `isError` with "instructions stale: call get_wiki_instructions".
  This is how a schema change cannot silently meet an old prompt.
- **One audit trail.** `wiki_runs.instructions_version` stores the version
  used for each run; `wiki_meta` stores the last version served.
- **One drift test.** A core test parses the instruction document and asserts
  it names every required frontmatter key, every page kind, both citation
  forms, the merge rule, the link rule, and every write tool. Bumping the
  schema or a tool without updating the document fails CI. The same test
  checks `WIKI_INSTRUCTIONS_VERSION` was bumped when the document hash
  changes.
- **One UI source.** The Setup panel renders the same embedded bytes, so what
  the user copies is what the server serves.

## 7. Schema and migration v12

New file: `migrations/0012_wiki.sql`. Additive only, `IF NOT EXISTS`, in the
style of `0011_project_paths.sql`.

```sql
CREATE TABLE IF NOT EXISTS wiki_meta (
    key   TEXT PRIMARY KEY NOT NULL,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS wiki_sources (
    rel_path    TEXT PRIMARY KEY NOT NULL,   -- relative to documents root
    kind        TEXT NOT NULL,               -- 'markdown' | 'pdf'
    hash        TEXT NOT NULL,
    size        INTEGER NOT NULL DEFAULT 0,
    mtime       INTEGER NOT NULL DEFAULT 0,
    state       TEXT NOT NULL DEFAULT 'new', -- new|indexed|compiled|stale|failed
    compiled_at INTEGER,
    error       TEXT
);

CREATE INDEX IF NOT EXISTS idx_wiki_sources_state ON wiki_sources (state);
CREATE INDEX IF NOT EXISTS idx_wiki_sources_hash  ON wiki_sources (hash);

CREATE TABLE IF NOT EXISTS wiki_pages (
    rel_path      TEXT PRIMARY KEY NOT NULL, -- relative to wiki root
    kind          TEXT NOT NULL,             -- source|entity|concept|index|page
    title         TEXT NOT NULL,
    summary       TEXT NOT NULL DEFAULT '',
    aliases_json  TEXT NOT NULL DEFAULT '[]',
    tags_json     TEXT NOT NULL DEFAULT '[]',
    reviewed      INTEGER NOT NULL DEFAULT 0,
    content_hash  TEXT NOT NULL,
    generated_hash TEXT,
    updated_at    INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX IF NOT EXISTS idx_wiki_pages_kind ON wiki_pages (kind);

CREATE TABLE IF NOT EXISTS wiki_page_sources (
    page_path   TEXT NOT NULL REFERENCES wiki_pages (rel_path) ON DELETE CASCADE,
    source_path TEXT NOT NULL,
    source_hash TEXT NOT NULL DEFAULT '',
    start_line  INTEGER NOT NULL DEFAULT 0,
    end_line    INTEGER NOT NULL DEFAULT 0,
    start_page  INTEGER NOT NULL DEFAULT 0,
    end_page    INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (page_path, source_path, start_line, end_line, start_page, end_page)
);

CREATE INDEX IF NOT EXISTS idx_wiki_page_sources_source
    ON wiki_page_sources (source_path);

CREATE TABLE IF NOT EXISTS wiki_links (
    from_path TEXT NOT NULL,
    to_path   TEXT NOT NULL,
    anchor    TEXT NOT NULL DEFAULT '',
    PRIMARY KEY (from_path, to_path, anchor)
);

CREATE INDEX IF NOT EXISTS idx_wiki_links_to ON wiki_links (to_path);

CREATE TABLE IF NOT EXISTS wiki_runs (
    id                   TEXT PRIMARY KEY NOT NULL,
    operation            TEXT NOT NULL,
    agent                TEXT NOT NULL DEFAULT '',
    instructions_version INTEGER NOT NULL DEFAULT 0,
    started_at           INTEGER NOT NULL DEFAULT 0,
    finished_at          INTEGER,
    stats_json           TEXT NOT NULL DEFAULT '{}',
    note                 TEXT,
    error                TEXT
);

CREATE INDEX IF NOT EXISTS idx_wiki_runs_started ON wiki_runs (started_at DESC);

CREATE VIRTUAL TABLE IF NOT EXISTS wiki_fts USING fts5(
    path UNINDEXED,
    title,
    summary,
    aliases,
    body,
    tokenize = 'unicode61'
);
```

Registration in `crates/core/src/db/migrations.rs`:

1. `pub const SCHEMA_V12_SQL: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../migrations/0012_wiki.sql"));`
2. Add `M::up(SCHEMA_V12_SQL)` after `M::up(SCHEMA_V11_SQL)`.
3. Change `SCHEMA_VERSION` from 11 to 12.
4. Add a v11→v12 test: existing boards/cards/projects untouched, new tables
   empty, `user_version = 12`, and `migrations().validate()` passes.

`bootstrap` already snapshots before a pending migration, so the first launch
after this change produces a pre-wiki snapshot. No other boot change.

## 8. Core modules and signatures

New directory `crates/core/src/wiki/`, matching the existing `db/` and
`services/` convention. Add `pub mod wiki;` and re-exports in `lib.rs`.

```
crates/core/src/wiki/mod.rs       types, roots, status, rebuild entry
crates/core/src/wiki/sources.rs   scan, hash, source state, read source
crates/core/src/wiki/pages.rs     frontmatter, page IO, slug/path rules
crates/core/src/wiki/index.rs     DB upserts, FTS5, index.md, status
crates/core/src/wiki/links.rs     wikilink parse, graph, backlinks
crates/core/src/wiki/lint.rs      rules and deterministic fixes
crates/core/src/wiki/runs.rs      run ledger
crates/core/src/wiki/dedup.rs     deterministic duplicate candidates
crates/core/src/wiki/pdf.rs       PDF text extraction bridge
crates/core/src/wiki/instructions.rs  embedded contract + version constant
crates/core/src/wiki/instructions.md  canonical agent instructions
```

Signatures (all synchronous, `&Connection` first, roots explicit; this matches
`documents.rs` and the current page-tree layer). In this module `root` always
means the wiki page root:

```rust
// mod.rs
pub fn resolve_wiki_root() -> Result<PathBuf>;
pub fn ensure_wiki_root() -> Result<PathBuf>;
pub fn status(db: &Connection, root: &Path) -> Result<WikiStatus>;
pub fn rebuild(db: &mut Connection, root: &Path, documents_root: &Path) -> Result<WikiRebuild>;

// sources.rs
pub enum SourceState { New, Indexed, Compiled, Stale, Failed }
pub struct WikiSource { pub rel_path: String, pub kind: SourceKind, pub hash: String,
    pub size: u64, pub mtime: i64, pub state: SourceState, pub compiled_at: Option<i64>, pub page_count: u32 }
pub fn list_sources(db: &Connection, state: Option<SourceState>) -> Result<Vec<WikiSource>>;
pub fn refresh_sources(db: &mut Connection, documents_root: &Path) -> Result<SourceDelta>;
pub fn read_source(documents_root: &Path, rel_path: &str, range: SourceRange) -> Result<SourceText>;
pub fn hash_file(path: &Path) -> Result<String>; // SHA-256 hex

// pages.rs
pub enum PageKind { Source, Entity, Concept, Index, Page }
pub struct WikiPage { pub rel_path: String, pub kind: PageKind, pub title: String,
    pub summary: String, pub aliases: Vec<String>, pub tags: Vec<String>,
    pub reviewed: bool, pub content_hash: String, pub generated_hash: Option<String>,
    pub sources: Vec<PageSource> }
pub fn parse_frontmatter(text: &str) -> Result<(Frontmatter, &str)>;
pub fn render_page(fm: &Frontmatter, body: &str) -> String;
pub fn read_page(root: &Path, rel_path: &str) -> Result<WikiPageText>;
pub fn write_page(db: &mut Connection, root: &Path, rel_path: &str,
    content: &str, expected: Option<&str>, force: bool, agent: &str) -> Result<WriteOutcome>;
pub fn patch_page(db: &mut Connection, root: &Path, rel_path: &str,
    old: &str, new: &str, expected: Option<&str>, force: bool, agent: &str) -> Result<PatchOutcome>;
pub fn delete_page(db: &mut Connection, root: &Path, rel_path: &str,
    expected: &str, force: bool, agent: &str) -> Result<()>;

// index.rs
pub fn index_page(db: &Connection, root: &Path, page: &WikiPage, body: &str) -> Result<()>;
pub fn index_all_pages(db: &mut Connection, root: &Path) -> Result<usize>;
pub fn rebuild_fts(db: &Connection, root: &Path) -> Result<usize>;
pub fn write_index_md(db: &Connection, root: &Path) -> Result<()>;
pub fn search(db: &Connection, query: &str, limit: usize) -> Result<Vec<SearchHit>>;

// links.rs
pub struct WikiLink { pub from: String, pub to: String, pub anchor: String }
pub fn parse_links(rel_path: &str, body: &str) -> Vec<WikiLink>;
pub fn resolve_links(db: &Connection, root: &Path, page: &WikiPage) -> Result<Vec<WikiLink>>;
pub fn backlinks(db: &Connection, rel_path: &str) -> Result<Vec<Backlink>>;

// lint.rs
pub fn lint(db: &Connection, root: &Path, scope: Option<&str>) -> Result<Vec<LintFinding>>;
pub fn fix(db: &mut Connection, root: &Path, codes: &[LintCode]) -> Result<Vec<FixedFinding>>;

// runs.rs
pub struct NewRun { pub operation: String, pub agent: String,
    pub instructions_version: u32, pub started_at: i64, pub finished_at: Option<i64>,
    pub stats: RunStats, pub note: Option<String> }
pub fn record_run(db: &Connection, run: NewRun) -> Result<String>;
pub fn list_runs(db: &Connection, limit: usize) -> Result<Vec<WikiRun>>;
pub fn finish_run(db: &Connection, id: &str, stats: &RunStats, error: Option<&str>) -> Result<()>;

// dedup.rs
pub struct DedupCandidate { pub existing_path: String, pub score: u32, pub reason: String }
pub fn candidates(db: &Connection, source_path: &str, limit: usize) -> Result<Vec<DedupCandidate>>;
```

Dependencies:

- Add `sha2 = "0.10"` to the workspace and `kandrium-core`. It is not in
  `Cargo.lock` today. `rand` cannot substitute for a stable content hash.
- Add `pdf-render = { path = "../pdf-render" }` to `kandrium-core` and a
  synchronous helper in `pdf-render`:

```rust
pub fn extract_text(path: &Path, first: Option<usize>, last: Option<usize>)
    -> Result<Vec<PageText>, String>;
```

  This reuses the existing `Pdf`/`group_lines` path without the worker. It
  keeps one PDF implementation instead of duplicating `hayro` in core.
  `pdf-render` has no UI dependency today, so the edge is safe.
  `wiki/pdf.rs` flattens each `PageText` by walking its lines and glyph
  indices in reading order, joining lines with `\n` and pages with
  `\n\n---\n\n`; the structured `PageText` stays available for future anchor
  refinement.
- No YAML crate. The subset parser is small and testable.
- No file-watching crate. Follow the existing `watcher.rs` polling pattern if
  a later stage needs one.

## 9. Pipeline stages in order

Each stage is independently shippable and leaves the app usable.

| Stage | Scope | Output | Verification |
|---|---|---|---|
| **0. Schema** | `0012_wiki.sql`, migration registration, core types, `wiki_meta` | Empty tables, `user_version = 12` | Migration test v11→v12; `migrations_validate`; existing tests green |
| **1. Sources** | Scan documents, SHA-256, upsert state, detect new/changed/deleted | `wiki_sources` populated; counts | Temp roots: add/modify/delete/rename; PDF + Markdown; hash stability |
| **2. Pages** | Parse the wiki page root, frontmatter subset, path rules, `wiki_pages`, FTS5, `index.md` | Pages indexed, searchable | Parse round-trip; unknown keys preserved; rebuild from files; FTS query |
| **3. Links** | Parse `[[...]]`, resolve aliases/slugs, `wiki_links`, backlinks | Graph and backlink queries | Self-links, fragments, display text, dead links, table/code exclusion |
| **4. Lint** | Rules in §11, findings only | Findings list | Fixture tree per code; no false positives on a clean wiki |
| **5. MCP read** | Tools up to `lint_wiki`, including `get_wiki_instructions` | External agent can read, lint, and fetch the contract | Router test; temp roots; path-escape rejects; PDF range caps; instructions round-trip |
| **6. MCP write** | `write`/`patch`/`delete`/`record_run`, optimistic hash, reviewed guard, `instructionsVersion` gate | Agent can build pages | Hash mismatch, reviewed rejection, stale-instructions rejection, atomic file, run row, index update |
| **7. UI** | Toolbar, panels, badges, watcher reload | Visible state and lint | gpui logic tests; manual agent round trip |
| **8. Dedup + instructions + setup** | Candidates, slug helper, embedded `instructions.md`, version constant, drift test, Setup panel | Agent merge support and a stable contract | Candidate ranking fixtures; instructions cover schema; Setup renders the same bytes |

Stage 0 rollback: revert the code; the DB stays at v12 with empty tables,
which is harmless because migrations are forward-only and additive. Later
stages never require destructive schema changes.

## 10. Incremental rebuild behaviour

### 10.1 Source states

| State | Meaning |
|---|---|
| `new` | Source discovered, never seen by the agent |
| `indexed` | Hash known, source available to read |
| `compiled` | At least one page cites it and hashes match |
| `stale` | Content hash changed since the citing page was written |
| `failed` | Extraction or validation failed (`error` holds the message) |

Transitions:

- Hash unchanged → state unchanged.
- New path → `new`.
- Changed hash → `stale`; every `wiki_page_sources` row with the old hash is
  stale.
- Missing path → delete `wiki_sources` row; pages citing it remain and lint
  reports `orphaned-by-source` / `missing-source`.
- Rename is delete + create. Do not guess. The agent re-links; lint reports
  the old citations.
- `record_wiki_run` may set `compiled_at` and `compiled` for cited sources, but
  only when the page's `source_hash` matches the current hash.

### 10.2 Page indexing

- `index_all_pages` walks the wiki root with `fsutil::scan_tree` (Markdown
  only), parses frontmatter, and upserts. It is cheap because it does not
  touch the documents root.
- `rebuild_fts` deletes and repopulates `wiki_fts` in one transaction. It is
  the recovery path, not the per-write path.
- `write_page` updates one `wiki_pages` row, its `wiki_page_sources`, its
  `wiki_links`, the FTS row, and `index.md`. All DB work is one
  `BEGIN IMMEDIATE` transaction after the file is atomically written.
- `patch_page` follows the same path; it is a read-modify-write with an exact
  `oldText` match.

### 10.3 Rebuild triggers

- First boot after v12: `wiki_meta.index_revision` is absent → full index.
- `WIKI_INDEX_REVISION` constant changes → full index at boot.
- User presses "Rebuild index" → full index.
- Section focus / explicit "Refresh" → incremental page scan + source hash
  scan.
- MCP `record_wiki_run` / page write → DB commit; the app's `data_version`
  watcher reloads trees and status.
- External editor writes a file without a DB commit → next refresh or section
  focus picks it up. v1 does not run a filesystem watcher.

### 10.4 Performance

- Hash files in a worker thread for the app; the MCP handler hashes on demand.
- Batch upserts in one transaction; cap a run at 500 changed sources before
  yielding.
- Skip FTS body indexing for pages above 1 MB; they stay readable but are
  searchable by title only.
- Never extract text from every PDF at index time. Extraction is per read.

## 11. Lint rules

| Code | Severity | Fixable | Meaning |
|---|---|---|---|
| `bad-frontmatter` | error | no | Compiled page missing `---`, `type`, or `title`; malformed array |
| `unmanaged-page` | warn | no | File under `sources/`, `entities/`, or `concepts/` without frontmatter |
| `dead-link` | error | yes | `[[target]]` resolves to no page or alias |
| `missing-alias` | warn | yes | Compiled page has no aliases |
| `orphan-page` | warn | no | No inbound links and not `index.md` |
| `empty-page` | warn | no | Body under 50 substantive characters |
| `missing-source` | error | no | Frontmatter cites a source absent from `wiki_sources` |
| `stale-page` | warn | no | A cited source hash differs from `source_hash` |
| `orphaned-by-source` | warn | no | The page's only source was deleted |
| `bad-citation` | error | no | Footnote anchor missing or outside the source |
| `duplicate-page` | warn | yes | Normalized title/alias collision; merge candidate |
| `contradiction` | info | no | `contradicted_by` is non-empty |
| `index-drift` | error | yes | `index.md` or DB rows disagree with the files |
| `pdf-unreadable` | error | no | PDF extraction failed (scanned image, corrupt) |
| `unrecorded-run` | info | no | Page changed without a matching `wiki_runs` row |

Fix order (causal, matching the upstream reference): aliases → duplicate merge
→ dead links → orphans → empty pages → index rebuild. "Fix" never invents
knowledge; it derives aliases from the title, creates stubs only when
configured, and otherwise reports.

## 12. UI surface inside the existing Wiki section

No new section, no new root, no change to `Section`. The section remains
`Tasks | Projects | Wiki | Clocks`.

### 12.1 Layout

`wiki::wiki_view` gains a 34 px toolbar above the tree/detail split:

```
[ Sources 12 · 3 stale · Pages 84 ]      [ Refresh ] [ Lint ] [ Setup ]
```

- `Refresh` re-scans sources and pages and reloads badges.
- `Lint` opens the lint panel and runs the checks.
- `Setup` opens the compile-setup panel: the MCP client entry, the embedded
  instructions (the same bytes `get_wiki_instructions` serves), and a copy
  button. This is the only "compile" affordance in v1.

Panels render in the detail column when `WikiState.panel` is set, with a close
button. They are not tabs, so `TabKind`, `TabKey`, `order`, and every existing
tab handler stay untouched.

### 12.2 State

`WikiState` gains:

```rust
pub panel: Option<WikiPanel>,           // Status | Lint | Setup
pub status: Option<WikiStatus>,         // counts, last run
pub findings: Vec<LintFinding>,
pub busy: Option<WikiBusy>,             // Refresh | Lint | Rebuild
```

`DocumentsState` gains `source_state: HashMap<PathBuf, SourceState>`, loaded
by `AppShell` from `wiki_sources`, so `documents::tree_body` keeps its
signature. Rows show a 3 px dot: gray new, blue compiled, amber stale, red
failed.

Wiki rows compute a "generated" mark from the `sources/`, `entities/`, or
`concepts/` prefix; no state field is needed. Hand-written pages elsewhere
under the root do not get the mark. The tab/status bar shows `N sources` for
a compiled page.

### 12.3 Refresh and external writes

- `wants_watcher()` adds `Section::Wiki`.
- `apply_external_change` handles `Section::Wiki`:
  `load_wiki_pages(); load_sources(); reload_wiki_status(); cx.notify();`
- `reload_wiki_status` reads counts and the last run from SQLite; it does not
  hash files.
- `select(Section::Wiki)` calls `load_wiki()` (the same reload path).
- The full source hash scan runs on app start, `Refresh`, and after a run
  that reports changed sources. It runs in a worker thread if it exceeds a
  small file count; the UI drains a channel, following `watcher.rs`.

### 12.4 Editor

Generated pages open in the existing page editor with vim mode. Frontmatter
is shown as raw text in v1; a collapsed frontmatter header is a later polish
item. Wikilinks render as plain text in v1; a clickable link style is optional
in stage 7.

## 13. Risks that would actually bite

| Risk | Why it bites | Mitigation |
|---|---|---|
| **Stale index** | DB says compiled while the file changed; search returns old text | `content_hash` and `source_hash`; `stale-page`/`index-drift` lint; `Refresh` and `Rebuild`; write path updates index in the same call |
| **Half-written pages** | A crash during a normal write truncates a page | Only `fsutil::write_atomic`; file first, DB second; unfinished `wiki_runs` rows; rebuild repairs |
| **Agent overwrites hand edits** | The agent has the same write power as the app | `reviewed: true` set on app save; `expectedHash`; `force` required and recorded; lint `unrecorded-run` |
| **Two agents race** | Last writer wins silently | `expectedHash` optimistic concurrency; `BEGIN IMMEDIATE`; per-file atomic writes; lint catches drift |
| **PDF sources** | Scanned PDFs have no text; extraction is expensive; page anchors drift | Text-layer only in v1; `pdf-unreadable`; per-call page caps; anchors are page numbers, not byte offsets; no OCR promise |
| **App does not see external writes** | Wiki is excluded from the DB watcher today | Include Wiki in `wants_watcher`; reload on `data_version`; manual refresh; no DB commit means no automatic update |
| **Hand-written pages inside compiled folders** | `sources/`, `entities/`, and `concepts/` are visible and writable | `unmanaged-page` lint; generated badge; never delete; document the convention in the Setup panel |
| **Path escape** | MCP input is untrusted text | `fsutil::guard` plus a `wiki/` prefix check; reject absolute paths, `..`, and symlinks; tests for each |
| **FTS5 availability** | A build without FTS5 fails the migration | Verified enabled in the bundled build; migration test creates the table; a startup error names the feature if missing |
| **Migration on a large DB** | A long migration blocks both app and MCP boot | Additive DDL only; `bootstrap` snapshot; `BEGIN` is short; FTS population is lazy (first rebuild), not in the migration |
| **Index rebuild blocks the UI** | Full rebuild over thousands of pages | Run in a worker with progress; batch transactions; incremental per-write path is the default |
| **Frontmatter corruption** | A malformed page is invisible or crashes lint | Parse errors become `bad-frontmatter`, never panics; unknown keys preserved; write path rejects malformed input before touching the file |
| **Instruction drift** | The agent's rules and the schema diverge, or a stale prompt keeps writing against a new contract | One embedded `instructions.md` and version constant; write tools require the current version; drift test fails CI when the two disagree; Setup renders the same bytes |
| **Source deletion removes knowledge silently** | Deleting a PDF can orphan pages | Never auto-delete pages; `orphaned-by-source`; user decides |

## 14. Open questions and defaults

| Question | Default | Revisit when |
|---|---|---|
| Where do generated pages live? | `wiki/{index.md,sources,entities,concepts}` | Users want compiled output outside the page root |
| Existing legacy page root: migrate or start empty? | `wiki/` starts empty; the legacy root is never moved automatically. Offer a one-time, explicitly confirmed copy into `wiki/imported/` with a backup. | Users want automatic migration |
| `KANDRIUM_NOTES` → `KANDRIUM_WIKI`? | Add `KANDRIUM_WIKI`; keep `KANDRIUM_NOTES` as a deprecated read alias for one release, then remove it. | A clean break is preferred |
| Page kinds? | `source`, `entity`, `concept`, plus generated `index` | A synthesis/overview kind is proven necessary |
| Citation format? | Footnotes with `path#Lx-Ly` or `path#page=N` | Anchors drift too often on PDFs |
| Does the app set `reviewed` on save? | Yes, automatically | Users want an explicit toggle |
| Delete pages when sources disappear? | No; mark and lint | A retention policy is requested |
| MCP write default? | `KANDRIUM_MCP_WIKI=write` | A read-only client becomes common |
| Separate MCP binary? | No; extend `crates/mcp` | Per-client tool isolation is required |
| Source search scope? | Pages via FTS5; sources on demand | Source search becomes hot |
| PDF OCR? | Out of scope; text layer only | Scanned sources are a core use case |
| Multiple documents roots? | One root | Projects need per-project sources |
| Frontmatter rendering? | Raw text in the editor | Users ask for a collapsed header |

## 15. Testing and verification

Per stage:

- `cargo fmt --check`; `cargo clippy --workspace`; `cargo test --workspace`.
- The gpui crate is a separate workspace:
  `cargo test --manifest-path crates/app-gpui/Cargo.toml`.
- Core tests use `tempfile::tempdir()` roots, never the real data directory.
- Migration: build a v11 DB with boards/cards/projects, run `bootstrap`, assert
  v12 tables exist, old rows unchanged, `wiki_fts` queryable.
- Frontmatter: round-trip, unknown keys, malformed input, CRLF.
- Path guard: `..`, absolute path, symlink, `wiki` root prefix, documents
  root read-only.
- Links: alias resolution, fragments, display text, code-fence and table
  exclusion, dead link.
- Lint: one fixture per code; clean-wiki test has zero findings; fix idempotence.
- MCP: router contains the original 13 plus the wiki tools; every write tool
  rejects a hash mismatch, a stale `instructionsVersion`, and a `reviewed`
  page; a write creates a run row and an FTS hit; a PDF read caps pages.
- Instructions: `get_wiki_instructions` returns the embedded bytes and current
  version; the drift test covers every required frontmatter key, page kind,
  citation form, merge rule, link rule, and write tool; the Setup panel output
  equals the server output.
- UI logic: toolbar counts, badge mapping, panel open/close; no duplicate
  widget IDs in headless runs.
- Manual round trip: add a PDF and a Markdown source, run an external agent
  against the MCP server, confirm pages, index, lint, and refresh in the app.

## 16. Out of scope

- In-app provider implementation (considered and rejected; see §2).
- OCR for scanned PDFs.
- Embeddings, vector search, or PPR beyond one-hop link expansion.
- Sync, team mode, or a hosted backend.
- Editing or moving source documents from the wiki.
- PDF annotation integration.
- Automatic background compilation.
- Renaming or moving hand-written pages outside the wiki root.
- A second MCP binary.

## 17. References

- GD4AI/obsidian-llm-wiki (Apache-2.0): page layout, frontmatter, ingest
  gates, duplicate tiers, link rules, lint categories, causal fix order.
- lucasastorian/llmwiki (Apache-2.0): raw-folder/`wiki/`/`index.db` shape,
  rebuildable index, watcher behaviour, MCP tool set, protected root pages.
- atomicstrata/llm-wiki-compiler (MIT): read-only reference for incremental
  state and review gates; no code reuse planned.
- Kandrium source inspected: `crates/core/src/{lib,documents,fsutil,error}.rs`,
  the current page-tree module, `crates/core/src/db/{migrations,connection}.rs`,
  `crates/mcp/src/main.rs`, `crates/app-gpui/src/views/wiki.rs`,
  `crates/app-gpui/src/app.rs`, `crates/app-gpui/src/watcher.rs`,
  `crates/pdf-render/src/lib.rs`, `migrations/0011_project_paths.sql`.
