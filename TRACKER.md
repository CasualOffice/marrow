# Tracker

**Current milestone:** M0 — Measure
**Last updated:** 2026-08-30

> This is the file that actually gets updated. [ROADMAP.md](ROADMAP.md) is the plan; this is the state.
> Convention: `[ ]` todo · `[~]` in progress · `[x]` done · `[-]` dropped (say why) · `[?]` blocked

---

## Progress

| M | Name | Status | Started | Done |
|---|---|---|---|---|
| M0 | Measure | `[~]` | 2026-08-30 | — |
| M1 | Index + query | `[ ]` | — | — |
| M2 | MCP server | `[ ]` | — | — |
| M3 | PDF + tables | `[ ]` | — | — |
| M4 | Semantic | `[ ]` | — | — |
| M5 | Write tools | `[ ]` | — | — |
| M6 | Timeline | `[ ]` | — | — |
| M7+ | Optional | `[ ]` | — | — |

---

## M0 — Measure

**Exit:** numbers written to `bench/M0-corpus.md`; first roots chosen.

- [ ] Count files by extension across candidate roots
- [ ] Count by size bucket (`<64KB`, `<1MB`, `<50MB`, `>50MB`)
- [ ] Identify cloud-sync roots and count placeholder/dehydrated files
- [ ] Note pathological cases: huge dirs, `node_modules`, build output, symlink loops
- [ ] Spike: `ignore` walk over the real home dir — time it, count errors
- [ ] Spike: `blake3` hash throughput on the same set
- [ ] Spike: SQLite batched insert rate on this machine
- [ ] Write `bench/M0-corpus.md`
- [ ] Decide first two workspace roots
- [ ] Record anything that contradicts the spec's assumptions in [DECISIONS.md](DECISIONS.md)

**Useful one-liners for the counts:**
```sh
# by extension, top 40
find ~ -type f 2>/dev/null | sed 's/.*\.//' | grep -v '/' | sort | uniq -c | sort -rn | head -40
# by size bucket
find ~ -type f -size +50M 2>/dev/null | wc -l
# macOS: dataless / iCloud placeholders
find ~ -flags dataless 2>/dev/null | wc -l
```

---

## M1 — Index + query

**Exit:** beats current search on your corpus · survives `kill -9` mid-scan · zero reconciliation drift over 72 h.

### Foundation
- [ ] Cargo workspace + crate layout
- [ ] `tracing` to file; error taxonomy skeleton ([Part 6 §108](docs/LKAR_Addendum_Part_6.md))
- [ ] SQLite: M1 table subset, WAL pragmas, single-writer actor
- [ ] Migration runner + `VACUUM INTO` backup before migrate
- [ ] Job queue: idempotency keys, leases, backoff, resume-after-crash

### Discovery
- [ ] Workspace + root model with explicit consent
- [ ] `ignore`-based scan, `.gitignore` aware, symlinks off
- [ ] `blake3` content hashing
- [ ] Stable `file_id` + path history (**path ≠ identity**)
- [ ] **Cloud placeholder detection — never hydrate** ⚠️
- [ ] `notify` watcher with coalescing/debounce
- [ ] Reconciliation loop; watcher-health reporting (`Live`/`Degraded`/`Poll-only`)
- [ ] Unicode NFC/NFD normalization so macOS doesn't create duplicate identities

### Parsing
- [ ] Parser trait + versioned IR with `source_span` on every node ⚠️
- [ ] Parser subprocess with timeout + memory cap
- [ ] Plain text, Markdown
- [ ] Code via Tree-sitter (start with the 3 languages you actually use)
- [ ] JSON / YAML / TOML
- [ ] CSV with dialect + encoding detection
- [ ] `META` extraction: EXIF, OOXML properties, download-origin xattr/ADS

### Index + query
- [ ] Tantivy schema and writer
- [ ] Near-real-time index updates after parse
- [ ] CLI `search` (filters: path, type, date)
- [ ] CLI `file` — the file-intelligence panel
- [ ] CLI `status` — index health, backlog, errors, placeholder count
- [ ] "Why not found" explanation

### Gates
- [ ] `kill -9` mid-scan → resumes cleanly, no duplicate work
- [ ] 72 h soak → reconciliation drift = 0
- [ ] Zero placeholder hydration confirmed on a real cloud folder
- [ ] Used daily for one week

---

## M2 — MCP server ⭐

**Exit:** you reach for it without thinking. **Keep to 1–2 weeks — cut tools, not time.**

- [ ] MCP server over stdio, stateless
- [ ] Tool: `search`
- [ ] Tool: `read` (with `source_span` in results)
- [ ] Tool: `stat`
- [ ] Tool: `file_intelligence`
- [ ] Tool: `list_workspaces`
- [ ] Results carry provenance class and citation handles
- [ ] Wire into Claude Code; use for one week
- [ ] Note which tool you actually used most → informs M3

---

## M3 — PDF + tables

- [ ] PDFium integration; text + page + bbox provenance
- [ ] Scanned-PDF detection (yield ≈ 0 with pages > 0) — flag, don't silently drop
- [ ] Table IR schema (`table_ir`, `table_cells`)
- [ ] CSV / Markdown / HTML tables
- [ ] XLSX via calamine — formulas, named ranges, number formats
- [ ] DOCX tables
- [ ] PDF ruled tables from line objects
- [ ] Header detection with confidence
- [ ] Column type inference; raw text always retained
- [ ] Unit extraction from headers/formats/captions
- [ ] `table compute`: sum / filter / group / lookup, citing cells
- [ ] Unit-mismatch blocks the operation (never coerce silently)
- [ ] Table chunks: repeated headers + schema chunk
- [ ] Expose over MCP

---

## M4 — Semantic

- [ ] Format-aware chunker with structural context prefix
- [ ] Chunk-stable IDs; IR diffing so unchanged chunks keep their vectors
- [ ] Embedding provider trait
- [ ] Local embeddings (Candle) or Ollama adapter — decide [D2/D31](DECISIONS.md)
- [ ] Vector storage: brute-force cosine first
- [ ] Content-addressed embedding cache
- [ ] RRF fusion; weights in config, not code
- [ ] `search --explain`
- [ ] Golden query set from your own queries (~30 to start)
- [ ] Regression check wired into CI

---

## M5 — Write tools

- [ ] Tool trait: risk, reversibility, validator, prepare/execute/verify
- [ ] Transaction + step + snapshot tables
- [ ] `filesystem.patch` with read-before-edit guard
- [ ] Stale-version check immediately before commit ⚠️
- [ ] Atomic write + snapshot + undo
- [ ] Validators: reparse, diff-match
- [ ] Structural edits: JSON/YAML/TOML/Markdown/code
- [ ] E1 recipe DAG: no loops, no dynamic code, resolves before executing
- [ ] Recipe re-resolves targets on every run
- [ ] **Adversarial corpus in CI — zero escapes** ⚠️ (must precede any write tool shipping)
- [ ] Expose write tools over MCP behind confirmation

---

## M6 — Timeline

- [ ] `activity_events` table
- [ ] File lifecycle events from watcher + reconciliation
- [ ] Git commit integration
- [ ] "What changed in X since Y" query
- [ ] Version diff across `FileVersion`s
- [ ] Expose over MCP

---

## Standing checks

Re-verify at every milestone exit:

- [ ] No secret, key or token in `settings.json` or any log
- [ ] Diagnostics contain no file bodies
- [ ] Every derived index rebuildable from canonical state
- [ ] Corrections survive a full derived rebuild
- [ ] Every mutation tool has a declared validator (from M5)
- [ ] Adversarial corpus green (from M5)
- [ ] Backup taken before any migration

---

## Adversarial corpus

Build these as fixtures. The set only grows — every security bug found adds a permanent case. Full list: [Part 6 §116.2](docs/LKAR_Addendum_Part_6.md).

- [ ] Hostile instruction inside a PDF
- [ ] README asking the agent to upload keys
- [ ] Symlink to `~/.ssh` inside a cloned repo
- [ ] Zip-slip archive
- [ ] Decompression bomb (archive + image)
- [ ] Command injection via filename
- [ ] Stale-file race before write
- [ ] Tool result containing fake approval text
- [ ] Injection inside a screenshot (OCR text)
- [ ] Injection inside an EXIF comment
- [ ] **Self-poisoning: agent writes a summary, then cites it back**
- [ ] Cloud placeholder touched by any code path
- [ ] Table with mismatched units summed

---

## Parking lot

Ideas that came up but aren't scheduled. Move to a milestone or delete — don't let this grow.

- _(empty)_

---

## Log

Short entries. What shipped, what surprised you, what changed.

| Date | Entry |
|---|---|
| 2026-08-30 | Spec complete (7 parts). Re-scoped for solo/open-source in Part 7. Repo initialised. M0 started. |
