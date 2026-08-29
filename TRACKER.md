# Tracker

**Current milestone:** M1 — Index + query
**Last updated:** 2026-08-30

> This is the file that actually gets updated. [ROADMAP.md](ROADMAP.md) is the plan; this is the state.
> Convention: `[ ]` todo · `[~]` in progress · `[x]` done · `[-]` dropped (say why) · `[?]` blocked

---

## Progress

| M | Name | Status | Started | Done |
|---|---|---|---|---|
| M0 | Measure | `[x]` | 2026-08-30 | 2026-08-30 |
| M1 | Index + query | `[~]` | 2026-08-30 | — |
| M2 | MCP server | `[ ]` | — | — |
| M3 | PDF + tables | `[ ]` | — | — |
| M4 | Semantic | `[ ]` | — | — |
| M5 | Write tools | `[ ]` | — | — |
| M6 | Timeline | `[ ]` | — | — |
| M7+ | Optional | `[ ]` | — | — |

---

## M0 — Measure

**Exit:** numbers written to `bench/M0-corpus.md`; first roots chosen.

- [x] Count files by extension across candidate roots — 189 exts, code+md+photos dominate
- [x] Count by size bucket — 70.6% <64KB, nothing ≥500MB
- [x] Identify cloud-sync roots and count placeholders — **58 files, 58 placeholders, 1.35 GB, measured in 9–36 ms** via `SF_DATALESS`. The M0 timeout was `find`'s traversal, not the flag check (F13–F18)
- [x] Note pathological cases — `node_modules` 76k files in 15 dirs; `~/Library` excluded by design; 0 symlinks
- [x] Spike: `ignore` walk — **97,308 files/s**, 0 errors
- [x] Spike: `blake3` — **417 MB/s**, 4,209 files/s, 3.6% dupes
- [x] Spike: SQLite batched insert — **234,725 rows/s** (spec estimated 5–20k)
- [x] Write [`bench/M0-corpus.md`](bench/M0-corpus.md)
- [x] Decide first roots → `~/Desktop` (the whole corpus, effectively), `~/Pictures`
- [x] Record contradictions → 12 findings F1–F12, 6 decisions moved

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
- [x] Cargo workspace + crate layout (`core`, `store`, `scan`, `cli`)
- [x] Error taxonomy ([Part 6 §108](docs/Part_6_Engineering_Reference.md)) — 28 codes, `POL_*` never retryable
- [x] Typed ULID IDs; `SourceSpan`, `TierState`, `Origin` encode invariants #1/#5/#13
- [x] `check.sh` + CI — fmt, clippy -D warnings, test, named invariant tests
- [x] SQLite: M1 table subset, WAL pragmas, single-writer actor (per-op `SAVEPOINT`, so one bad row doesn't discard 499 good ones)
- [x] Migration runner + `VACUUM INTO` backup before **every** migration, restore-on-failure
- [x] Job queue: idempotency keys, leases, backoff+jitter, resume-after-crash
- [x] `origin_device_id` on canonical tables ([D49](DECISIONS.md)) — retrofit-expensive, done now
- [x] Readers are `query_only=ON` — "never open a second write connection" enforced by SQLite
- [ ] `tracing` subscriber wiring in the CLI

### Discovery
- [ ] Workspace + root model with explicit consent
- [x] `ignore`-based scan, `.gitignore` **as per-root policy** (D47), symlinks off
- [x] `blake3` content hashing — refuses non-Resident files, unreachable without a tier check
- [x] **Cloud placeholder detection — never hydrate** ⚠️ — `SF_DATALESS` + `.icloud` stubs, metadata-only
- [x] Path canonicalization + symlink escape + NFC/NFD identity — component-wise, not string-prefix
- [x] Lazy walk with per-entry error isolation (FS-011)
- [ ] Stable `file_id` + path history (**path ≠ identity**) — store + scan both ready, needs the ingest pipeline to join them
- [ ] `notify` watcher with coalescing/debounce
- [ ] Reconciliation loop; watcher-health reporting (`Live`/`Degraded`/`Poll-only`)
- [ ] Unicode NFC/NFD normalization so macOS doesn't create duplicate identities

### Parsing — ordered by M0 file counts, not by spec order
- [ ] Parser trait + versioned IR with `source_span` on every node ⚠️
- [ ] Parser subprocess with timeout + memory cap
- [ ] Tree-sitter: Rust, TypeScript/TSX, JavaScript, Python, SQL (~1,300 files)
- [ ] Plain text (449)
- [ ] Markdown (289)
- [ ] TOML / JSON / YAML (~165)
- [ ] Image `META` — EXIF/XMP, metadata only, never decode pixels (3,478 files, 35% of corpus)
- [ ] CSV with dialect + encoding detection (~90)
- [ ] HTML / CSS (~94)
- [ ] XLSX / DOCX (66 — low priority, may slip to M3)
- [-] PDF — **dropped**, 14 files in the entire home dir (M0 F3)
- [ ] Default-exclude app noise: `*.dat` `*.toc` `*.journal` `*.strings` `*.plist`, fonts (~2,700 files, 29%)
- [ ] `.gitignore` respect as **per-root policy**, not global (M0 F9 — it hides 442 xlsx)

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

Build these as fixtures. The set only grows — every security bug found adds a permanent case. Full list: [Part 6 §116.2](docs/Part_6_Engineering_Reference.md).

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

- **Hydration path** ([D51](DECISIONS.md)) — opt-in, size shown, rate-limited, cancellable, battery/metered aware. Not M1; the indexer never hydrates regardless.
- Wire `PathId`/`ParseId`/`DeviceId` through `marrow-store` (currently raw `Ulid` at two call sites).
- `integrity_check` on unclean shutdown — belongs with the CLI `status` work.
- `chunks.provenance_class` CHECK constraint — when M4 first writes chunks.

---

## Log

Short entries. What shipped, what surprised you, what changed.

| Date | Entry |
|---|---|
| 2026-08-30 | Spec complete (7 parts). Re-scoped for solo/open-source in Part 7. Repo initialised. M0 started. |
| 2026-08-30 | Named **Marrow** (D45). Docs renamed `Part_N_*.md`. Crate namespace left open as D46. |
| 2026-08-30 | Reclaimed **64 GB** of Rust `target/` output (14→78 GB free). Build artifacts were 6.6× the entire knowledge corpus. M0 F11 superseded. |
| 2026-08-30 | **`marrow-store` done.** 36 tests. Found 6 spec defects ([D50](DECISIONS.md)); the load-bearing one: §106.1 mandates `origin_device_id`/`origin_principal_id` on every mutable row and the DDL declares neither. Settled as [D49](DECISIONS.md) and applied now — a column across 11 tables is not something to retrofit. |
| 2026-08-30 | **`marrow-scan` done.** 42 tests. Resolved M0's blocked item: 58 iCloud placeholders, 1.35 GB, 9–36 ms. Two traps found — `hidden(true)` would have hidden every placeholder (TIER-008 would read zero); APFS is normalization-*insensitive*, so NFC/NFD must be handled in our path key. D47 quantified: **34,459 files with gitignore off vs 9,435 with it on** — size M1 for ~34k. |
| 2026-08-30 | **Design pass.** GUI.md / UX.md / LLD.md written before more code. Five desktop screens mocked. D42 reversed — the desktop app is the product. |
| 2026-08-30 | **M1 started.** Workspace + `marrow-core` committed: 17 tests, clippy clean. Found ULID ordering is ms-granular not total — doc corrected, limitation pinned by a test. `store` and `scan` building in parallel. |
| 2026-08-30 | **M0 done.** Corpus is 9,435 files / 1 GB — 10× smaller than the spec assumed. Perf beats every target by 1–3 orders of magnitude. 14 PDFs total → PDF dropped from M3. Zero video/audio/email. See [bench/M0-corpus.md](bench/M0-corpus.md). |
