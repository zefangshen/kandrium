# Kandrium wiki instructions

You are compiling a local knowledge base. Documents under the documents root
are the raw sources. Pages under the wiki root are the compiled knowledge.
Kandrium owns hashing, the index, links, lint, and atomic writes; you own
reading the sources and writing good pages.

Read these instructions every run through `get_wiki_instructions`. Do not work
from a cached copy: the version and schema version can change.

## Workflow

1. `get_wiki_status` to see how much is indexed, then `list_wiki_sources`
   with `state: "new"` or `state: "stale"` to see what needs work.
2. `read_wiki_source` to read a source. Markdown is read by line, PDFs by
   page; pass `startLine`/`endLine` or `startPage`/`endPage` and follow
   `nextPage` while `truncated` is true.
3. `search_wiki` and `list_wiki_pages` before writing, so you merge instead of
   duplicating.
4. `read_wiki_page` before editing an existing page. It returns the current
   content hash, citations, and backlinks.
5. Write with `write_wiki_page`, `patch_wiki_page`, or `delete_wiki_page`.
   Pass `expectedHash` when you edited a page you read, so a concurrent user
   edit is not overwritten.
6. `record_wiki_run` once at the end with the pages you touched and the
   sources you compiled.
7. `lint_wiki` to confirm the result.

## Where pages live

- `sources/<slug>.md` — one page per source document, summarising it.
- `entities/<slug>.md` — people, organizations, products, named things.
- `concepts/<slug>.md` — ideas, methods, techniques, recurring themes.
- Anything else under the wiki root is hand-written by the user. Read it, but
  do not create new hand-written folders.

Use lowercase kebab-case slugs (`attention-is-all-you-need`). One page per
topic. Never write `index.md`: Kandrium regenerates it.

## Frontmatter

Every compiled page starts with a YAML block:

```yaml
---
type: concept
title: Self-Attention
summary: One sentence that tells a reader what this page answers.
created: 2026-11-14
updated: 2026-11-14
sources: [papers/attention-is-all-you-need.pdf]
tags: [ml, method]
aliases: [self attention, scaled dot-product attention]
reviewed: false
contradicted_by: []
---
```

Required for compiled pages:

- `type`: exactly `source`, `entity`, or `concept`, matching the folder.
- `title`: the canonical human name.
- `summary`: one sentence, no trailing newline.
- `sources`: document-relative paths used by the page.

Optional:

- `aliases`: alternate names, spelling variants, and abbreviations a reader
  might search for. Include at least one alias on every compiled page.
- `tags`: only values from the vocabulary returned by
  `get_wiki_instructions`.
- `created` / `updated`: `YYYY-MM-DD`.
- `contradicted_by`: page paths that disagree with this page.
- `reviewed`: the user's flag. Never set it to `true`; never change it.

Keep unknown keys exactly as you found them. Do not reformat a page you are
only patching.

## Citations

Every factual claim must point at the lines or pages it came from. Use
footnotes at the end of the body:

```markdown
Self-attention relates every token to every other token.[^1]

[^1]: documents/papers/attention-is-all-you-need.pdf#page=3
[^2]: documents/readings/notes.md#L12-L18
```

Rules:

- Line ranges are 1-based and inclusive. `#L12-L18` is seven lines.
- PDF anchors are `#page=N`, 1-based.
- The path is relative to the documents root. A leading `documents/` is
  accepted and normalised away.
- Do not cite a line range you did not read.
- Do not cite a whole PDF when a page or range is enough.
- Do not invent a citation to make a page look sourced. If the source does not
  support the claim, leave the claim out.

## Creating versus merging

Before creating a page, search for the title, aliases, and related terms.

- **Merge** when the new material is about the same entity or concept. Append
  the new facts and their citations, extend `sources`, and add aliases. Keep
  existing prose; do not rewrite it wholesale.
- **Create** when the topic is distinct and no page owns it.
- **Link** the new page from the most related existing page, and link back.
- If two pages describe the same thing, merge into the better-named page and
  add the other title as an alias. Use `list_wiki_dedup_candidates` to find
  likely duplicates.
- If a source contradicts a page, add the page to `contradicted_by` rather
  than deleting the older claim.

## Links

Link with double brackets, root-relative, without the `.md` extension:

```markdown
See [[concepts/self-attention|Self-Attention]] and [[entities/vaswani]].
Use [[concepts/self-attention#Why it works]] for a section.
```

- Link the first mention of a related page, not every mention.
- Never link a page to itself.
- Never put wikilinks in code blocks, inline code, tables, image alt text, or
  an existing Markdown link label. They are ignored by the resolver.
- Never link to a page that does not exist; create it or use plain text.

## What you must not do

- Do not edit, move, rename, or delete a document. Documents are read-only.
- Do not write outside the wiki root, and never above it.
- Do not write `index.md`, `.kandrium.json` sidecars, or non-Markdown files.
- Do not overwrite a page whose `reviewed` flag is `true` without explicit
  user confirmation through `force`.
- Do not rewrite unrelated pages, reorder frontmatter keys, or reformat the
  user's hand-written pages.
- Do not set `reviewed`.
- Do not fabricate citations, titles, aliases, or prose. Say less if the
  source is thin.

## Instructions versioning

Every write takes `instructionsVersion`; send the value from
`get_wiki_instructions`. Kandrium rejects a stale version so a page cannot be
written against an outdated schema. `get_wiki_instructions` also returns
`schemaVersion`, `instructionsHash`, and the tag vocabulary; use the hash to
detect that the text changed mid-run.
