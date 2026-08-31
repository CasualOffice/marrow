# Roadmap

Milestones, not phases. Each one ends at something used daily. Derived from [Part 7 §131](docs/Part_7_Solo_Rescope.md).

**Estimates are solo: (part-time / focused full-time).** They are estimates, not commitments — see [Stop rules](#stop-rules).

---

## Overview

| M | Name | Effort | Ends when |
|---|---|---|---|
| **M0** | Measure | 1 wk / 2 d | You know your real corpus numbers |
| **M1** | Index + query | 6–10 wk / 3–4 wk | You use it instead of `grep`/Spotlight |
| **M2** | **MCP server** | **1–2 wk / 3–5 d** | **Your coding agent can search everything you own** |
| **M3** | PDF + tables | 4–7 wk / 2–3 wk | Cited numbers out of spreadsheets and PDFs |
| **M4** | Semantic | 4–6 wk / 2–3 wk | Conceptual search beats lexical on your own queries |
| **M5** | Write tools | 4–6 wk / 2–3 wk | You let an agent edit *through* Marrow, not around it |
| **M6** | Timeline | 3–4 wk / 1.5–2 wk | You can reconstruct last month |
| **M7+** | Optional | open | Only what you actually miss |

| Cumulative | Part-time | Full-time |
|---|---|---|
| **M2 — useful daily (via Claude Code)** | **7–12 weeks** | **4–5 weeks** |
| **M3 — the app exists** | **13–21 weeks** | **7–9 weeks** |
| M4 — semantic working | 17–27 weeks | 9–12 weeks |
| M6 — the substantive product | 24–37 weeks | 12–18 weeks |

---

## M0 — Measure

**Goal:** replace the specification's assumptions with facts about your actual disk.

- Count files by extension, size bucket and directory
- Identify which roots live in cloud-sync folders (OneDrive / iCloud / Dropbox / Google Drive) and how many files are placeholders
- Spike: `ignore` walk + `blake3` hash + SQLite insert over your real home directory. Time it. Find where it's slow and where it's wrong

**Why first:** the spec sizes everything against 100k synthetic files. Your real corpus is smaller, weirder, and will reorder your parser priorities more than any table in the docs.

**Exit:** a written note of your numbers in `bench/M0-corpus.md`, and a decision on which roots to index first.

---

## M1 — Index + query

**Goal:** a searchable, self-maintaining index. No LLM anywhere.

- Workspaces and roots with explicit consent
- Recursive scan honouring `.gitignore` and exclude policy; symlinks off by default
- `notify` watcher + reconciliation loop; degraded-mode reporting
- **Cloud placeholder detection — never hydrate** (highest-severity item in M1)
- SQLite schema, M1 subset (see [Schema staging](#schema-staging)); single-writer actor
- Durable job queue: idempotent, leased, resumable
- Parsers: plain text, Markdown, code (Tree-sitter), JSON/YAML/TOML, CSV
- `META` extraction — EXIF, OOXML properties, download-origin xattr/ADS
- Tantivy lexical index
- CLI: `search`, `file`, `status`, `workspace add/list`

**Exit:** it beats your current search on your own corpus, survives a `kill -9` mid-scan, and reconciliation drift is zero after 72 hours.

---

## M2 — MCP server ⭐

**Goal:** the forcing function. Everything before this is unproven; everything after is informed by real use.

- MCP server over stdio
- Tools: `search`, `read`, `stat`, `file_intelligence`, `list_workspaces`
- Wire it into Claude Code and use it for a week

**Exit:** you reach for it without thinking. Keep this milestone at 1–2 weeks — if it slips, cut tools, not time.

---

## M3 — PDF + tables

- PDFium text extraction with page + bbox provenance
- Unified Table IR for CSV, XLSX (calamine + formulas), Markdown, HTML, DOCX
- Column type and unit inference; header detection with confidence
- `table compute` — sum/filter/group/lookup evaluated over the IR, citing cells
- Table chunks: repeated headers + a schema chunk
- Expose tables and file intelligence over MCP

**Exit:** you ask a numeric question about a spreadsheet and get a figure with cell citations you can verify.

---

## M4 — Semantic

- Format-aware chunking with structural context prefixes
- Local embeddings (Candle, or an installed Ollama)
- Vector storage — brute-force cosine first; LanceDB only when it hurts
- RRF hybrid fusion with per-branch weights in config
- `search --explain` showing branch ranks and multipliers
- Golden query set built from your own queries; regression check

**Exit:** conceptual search returns things lexical search misses, measurably, on your golden set.

---

## M5 — Write tools

- `filesystem.patch` with read-before-edit guard and stale-version check
- Transaction snapshots, validators, undo
- Structural edits for JSON/YAML/TOML/Markdown/code
- E1 recipes: declarative DAG over typed tools, previewed and reversible
- Expose write tools over MCP behind confirmation
- **Adversarial corpus wired into CI before any write tool ships**

**Exit:** you let an agent edit files through Marrow because the undo is better than editing directly.

---

## M6 — Timeline

- File lifecycle events; Git commit integration
- "What changed in X last month" queries
- Version diffing across `FileVersion`s

**Exit:** you can reconstruct what happened to a project without opening Git.

---

## The core, and why it is not optional

The product's premise is answering questions with citations over your own
files. Lexical search is not that. Three things stand between here and the
premise, and they share one dependency:

```
        model runtime  (Part 8)
         │      │      │
   embeddings  gen   structured output
         │      │      │
    semantic   Ask    graph extraction
```

So the runtime is built first — not as a detour, but because the other three
cannot start without it. Part 8 §150 stages it S1–S6, and S1 (a Models page
that correctly says what this machine can run, before anything downloads) is
the part that must be right before a single byte is fetched.

## M7+ — Optional

Build only what you miss. In rough order of likely value:

| | Gate |
|---|---|
| Screenshot OCR | You lose an answer that was in an image |
| Local LLM for summaries | You want summaries without a cloud round-trip |
| Knowledge graph + entities | **[D43](DECISIONS.md) — name three questions you actually asked that needed it** |
| Chart rendering (G1) | You want to see a table, not read it |
| ~~Desktop UI~~ | **Moved out of M7+.** [D42](DECISIONS.md) reversed the deferral and the shell shipped under M3 — the desktop app is the product, so its work is not optional and does not wait for the CLI to annoy anyone. Open desktop work lives in [BUGS.md](BUGS.md) and the TRACKER parking lot, not here |
| T3 sidecar (MarkItDown) | You hit formats you need and can't read |
| Email, video, audio | You personally need them |

---

## Schema staging

Don't build all 40 tables from [Part 6 §106](docs/Part_6_Engineering_Reference.md) up front. Carrying unused tables slows you down.

| Milestone | Tables |
|---|---|
| **M1** | `schema_meta`, `devices`, `workspaces`, `workspace_roots`, `files`, `file_paths`, `file_versions`, `parse_results`, `ir_nodes`, `chunks`, `jobs` |
| M3 | `table_ir`, `table_cells` |
| M4 | `embedding_models`, `vector_generations`, `chunk_vectors`, `index_generations` |
| M5 | `action_transactions`, `action_steps`, `action_snapshots`, `recipes`, `audit_events` |
| M6 | `activity_events` |
| M7 | `entities`, `entity_aliases`, `entity_merges`, `mentions`, `evidence`, `relations`, `facts`, `communities`, `summaries`, `corrections`, `media_derivatives` |

**But:** put `source_span` on `ir_nodes`, and `origin`/`content_hash`/`supersedes` on files and versions, from M1. Those are the retrofit-expensive ones.

---

## Scope rules

1. **Ship M2 before anything clever.** An index you query daily teaches you more than planning does.
2. **Add a parser the week you hit a file you wanted and couldn't read.** Never speculatively.
3. ~~**No UI until the CLI annoys you.**~~ Reversed by [D42](DECISIONS.md). The replacement rule is narrower and still says no to most things: **a desktop feature earns a place if it makes a citation easier to reach or easier to trust.** That is what separates inline document scoping (it stops one answer drawing on four unrelated services) from a tool catalogue or a calendar — see [docs/RESEARCH_LANDSCAPE.md](docs/RESEARCH_LANDSCAPE.md) §6.
4. **No knowledge graph until [D43](DECISIONS.md) passes.** It's the highest-probability, highest-impact risk in the spec and the easiest thing to build badly.
5. **No abstraction for platforms you don't run.** One OS.
6. **No performance work without a benchmark** against your own corpus.

## Stop rules

- If a milestone exceeds **2× its estimate**, cut its scope. Don't extend it.
- If you're three milestones deep and haven't used the thing in a week, you built the wrong milestone.
- **S1 (scope collapse) is the top risk.** The spec is seven parts long and you are one person. Every scope decision defaults to "no" until M2 ships.

## Definition of done, per milestone

| Every milestone |
|---|
| Used for real, on your own corpus, for at least a week |
| Tests green, including the adversarial corpus (from M5, non-negotiable) |
| TRACKER updated; anything learned that contradicts the spec written into DECISIONS |
| No half-finished subsystem carried into the next milestone |
