# Tracker

**Current milestone:** M3 — Desktop shell · Part 8 S1–S3 done, S4 next
**Last updated:** 2026-08-30

> This is the file that actually gets updated. [ROADMAP.md](ROADMAP.md) is the plan; this is the state.
> Convention: `[ ]` todo · `[~]` in progress · `[x]` done · `[-]` dropped (say why) · `[?]` blocked

---

## Progress

| M | Name | Status | Started | Done |
|---|---|---|---|---|
| M0 | Measure | `[x]` | 2026-08-30 | 2026-08-30 |
| M1 | Index + query | `[x]` | 2026-08-30 | 2026-08-30 |
| M2 | MCP server | `[x]` | 2026-08-30 | 2026-08-30 |
| M3 | Desktop shell | `[~]` | 2026-08-30 | — |
| M4 | Semantic | `[ ]` | — | — |
| M5 | Write tools | `[ ]` | — | — |
| M6 | Timeline | `[ ]` | — | — |
| M7+ | Optional | `[ ]` | — | — |

### Part 8 — model runtime ([§150](docs/Part_8_Model_Runtime.md))

The core. Semantic search, Ask and graph extraction all run through it,
so this is a dependency rather than a detour.

| S | Name | Status | Notes |
|---|---|---|---|
| S1 | Hardware probe + live sampler + sizing + Models page | `[x]` | `marrow-hw`, and the page reads it |
| S2 | Registry, catalogue, download, Ollama detection | `[x]` | Pinned against real manifests; a 212 MB download verified end to end |
| S3 | Supervisor: states, admission, breaker, queue, scratch | `[x]` | `marrow-model`. 92 tests |
| S4 | MLX runtime in a worker process, KV reuse, Fast/Thorough | `[x]` | Real answers; 81% prefix reuse on a follow-up |
| S5 | Ask pipeline (§148), streaming, Markdown/Mermaid/HTML | `[x]` | A conversation, not a one-shot box |
| S5b | Eval harness across the shortlist (§149) | `[ ]` | Two measurements taken; no harness yet |
| S6 | Cloud providers behind the same trait | `[ ]` | — |
| S7 | Creation tools (file, mermaid, html) | `[x]` | Wired to MCP; `origin = SELF` persisted and survives a reindex |
| S8 | Fetch and research | `[~]` | Wired to MCP; **confirmation prompt and multi-step research left** |

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
- [x] Stable `file_id` + path history (**path ≠ identity**) — rename keeps its id; hardlinks stay distinct
- [x] `notify` watcher with coalescing/debounce — 300 ms window, 4096-path batch cap
- [x] Watcher health `live`/`degraded`/`poll-only`, never silent (WATCH-009)
- [x] Adaptive sweep interval: 6 h live → 15 min degraded → 5 min poll-only (WATCH-010)
- [x] `marrow watch` — incremental indexing; a new file is searchable seconds later
- [ ] Watch several roots at once (one thread per watcher + a shared cancel)
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

### Ingest pipeline
- [x] Staged pipeline, bounded channels, N hash workers ([LLD §2.6](docs/LLD.md))
- [x] Cancellation token checked at every stage boundary
- [x] Progress counters (discovered / hashed / stored / skipped / failed)
- [x] **Idempotent: converges on a real 41,110-file corpus, 0 writes on re-run**
- [x] Wire `marrow-parse` behind the hash stage
- [x] Wire `marrow-index` behind parse
- [x] Chunking ([Part 6 §112](docs/Part_6_Engineering_Reference.md)) — structural boundaries, context prefix instead of overlap

### Index + query
- [x] D3 settled → SQLite FTS5, on transactional-consistency grounds
- [x] FTS5 adapter behind the `TextIndex` port — 45 tests, benchmarked
- [x] Literal search (CAP-005) — index-independent, cancellable, refuses non-Resident files
- [x] Migration composition at the binary, not in store (avoids the cycle)
- [x] Wire the index into the ingest pipeline
- [x] CLI `search` — jumpable `path:line`, breadcrumb, workspace-relative paths
- [ ] Search filters (path, type, date) — the port supports them, the CLI does not expose them yet
- [ ] CLI `file` — the file-intelligence panel
- [x] CLI `status` — workspaces, file counts, bytes, cloud-only
- [x] CLI `workspace add` / `list`, `index`
- [x] `--json` on every command; colour auto-off when piped
- [ ] "Why not found" explanation
- [ ] Real SIGINT handler (currently the process default; safe to resume, but a UX gap)

### Gates
- [ ] `kill -9` mid-scan → resumes cleanly, no duplicate work
- [ ] 72 h soak → reconciliation drift = 0
- [ ] Zero placeholder hydration confirmed on a real cloud folder
- [ ] Used daily for one week

---

## M2 — MCP server ⭐

**Exit:** you reach for it without thinking. **Keep to 1–2 weeks — cut tools, not time.**

- [x] MCP server over stdio, stateless — 28 tests
- [x] Tool: `search` — filters pushed into the query, not applied to results
- [x] Tool: `read_file` — refuses unindexed and cloud-only files
- [x] Tool: `file_info` — identity, hash, path history, tier, index state
- [x] Tool: `list_workspaces`
- [x] Tool: `index_status` — cloud-only count always present, never a silent zero
- [x] Results carry `provenance`, `origin` and a `citable` flag
- [x] Both `initialize` and `server/discover` handshakes accepted
- [x] `.mcp.json` written; verified working from an arbitrary cwd
- [ ] Use it for a week; note which tool you actually reach for → informs M3
- [ ] `search --literal` over MCP — `marrow-index::literal` exists, the tool does not expose it yet (removed from the schema rather than advertised-and-ignored)
- [x] `marrow-query` landed (32 tests) — read path: RRF fusion shape, FI read model, explain
- [ ] Swap MCP's and desktop's hand-rolled SQL for `marrow-query`

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

## Part 8 — model runtime

**Exit:** a question asked in the desktop app is answered by a local model,
grounded in retrieved chunks, with citations — and the app costs nothing when
nobody is asking.

### S1 — hardware `[x]`
- [x] `Probe` — static machine shape, pessimistic on failure (HW-010)
- [x] `Sampler` — live conditions under a 1 ms budget, ring buffer, staleness detectable
- [x] `sizing` — weights · KV · runtime buffers · resident embedder · OS reserve, calibrated to the 3–4 GB budget and pinned by a test
- [x] `profile` — Efficient / Balanced / Larger local / Cloud, defaulting by probe
- [x] Models page reading all of the above — machine, live memory, tiering, per-model verdict with the arithmetic

### S2 — registry `[~]`
- [x] `Entry`, `Capabilities`, `Licence`, `Source`, `Format`
- [x] Built-in catalogue: Qwen 3.5 4B · Nemotron Nano 4B · Granite 4 3B · Gemma 4B
- [x] **Real digests, pinned to commits.** Six models, every file carrying its own published SHA-256; the small files HuggingFace publishes only a git SHA-1 for were fetched and hashed. Generated by `pin-catalogue.py`, never typed
- [x] `kv_bytes_per_token` read from each model's own config. It replaced an estimate that under-predicted by up to 2×
- [x] Resumable download into `partial/`, per-file verified, promoted by one rename
- [x] Verified against the real network: 212 MB, 12 files, all digests matched
- [x] Ollama / LM Studio detection (R1 — zero bytes downloaded), verified by talking to the server rather than looking for a binary

### S3 — supervisor `[x]`
- [x] Lifecycle states with a reason on every transition (SUP-001/002/003)
- [x] Admission against the live sample; policy before resources; every refusal names its number
- [x] Circuit breaker, persisted, ladder 3/5/8
- [x] Bounded per-model queue, strict priority, cancellation both directions
- [x] Model workspace: content-addressed weights, per-request scratch, path-escape refusal, orphan sweep
- [x] KV prefix cache accounting: exact-prefix, scope-fenced, LRU under a footprint-derived cap
- [x] Supervisor thread with sampler-paced wakeups and a flushing shutdown

### S4 — first runtime `[x]`
- [x] `GenerationProvider` trait, one shape for local and cloud
- [x] MLX in a worker process over JSON Lines; an OOM kills the worker, not the index
- [x] The context envelope (§114) — per-session delimiter, collision regeneration, untrusted never last
- [x] KV prefix reuse: 604-token prompt, 487 cached on a follow-up
- [x] Fast / Thorough wired to the model's own reasoning flag and billed apart
- [x] Memory budget on the worker, enforced between tokens. Not an `rlimit`: `RLIMIT_AS` is useless against MLX on unified memory, so the guard watches resident footprint and kills on three consecutive breaches
- [ ] CPU share — not enforced; the load ceiling in admission defers work instead

### S5 — the ask surface `[x]`
- [x] Streaming to the window over a Tauri channel, token by token
- [x] A conversation: turns kept, history sent back, per-turn sources and reasoning
- [x] Markdown rendered and sanitised; `mermaid` fences drawn; `html` fences run in a sandboxed frame with no same-origin access
- [x] Citations clickable, self-written sources listed as excluded, egress disclosed
- [ ] Skeletons on the *search* path (SKEL-001..008) — only the Ask path streams

### S7 — creation tools `[~]`
- [x] **The adversarial corpus** — 59 cases in `corpus/adversarial/`, all
  exercised. The TOCTOU coverage was mutation-tested: disabling the pre-write
  re-canonicalisation lets a symlink created between validation and write
  escape the workspace, and the case goes red
- [x] One guarded write path: canonicalize at operation time, refuse excluded
  and protected subtrees, stale-check at commit, atomic rename, `origin = SELF`
- [x] `create_file` / `create_diagram` / `create_page`
- [x] **MCP wiring, including the part it must not skip.** Migration 3 adds
  `self_written`, keyed on content hash; the handler records a row before it
  reports success; ingest reads the set once per run and sets `files.origin`
  and the index doc's origin from it. Six tests cover it, including that the
  record survives a reindex and that a file the user edits becomes theirs again
- [x] `create_diagram` now refuses prose. It documented "starting with its
  type" and accepted anything non-empty

### S8 — fetch and research `[~]`
- [x] **The policy**, written first: [Part 9](docs/Part_9_Egress.md), 63 `NET-`
  requirements. HTTPS only, port 443 only, no persisted allow-list, and
  100.64.0.0/10 refused — which breaks Tailscale hosts on purpose
- [x] `marrow-net`: resolves the host and checks the **resolved IP**, re-checks
  on every redirect, caps bytes, time and hops, and returns what was disclosed
- [x] 44 of 63 requirements have a named test; 14 are structural; 3 are
  deferred to the UI; 2 are implemented and honestly marked untested
- [x] MCP wiring. A fetch needing confirmation is **refused** over MCP rather
  than silently granted — there is no one to ask on that surface, and treating
  that as consent would make the rule decorative
- [ ] The confirmation prompt itself (NET-018/023/024/050/058), which needs a
  surface that can ask
- [ ] Multi-step research on top of it

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
| 2026-08-30 | **Desktop UI lands and the app runs.** Three-pane window, quick-find, status, zero-results diagnosis; light+dark, WCAG AA, virtualized, selection anchored to identity so a re-rank never moves the row under the cursor. Closed four contract gaps the UI surfaced: `open_path`/`reveal_path` (↵ and ⇧↵ did nothing), `matched` vs `total` (the footer reported page size as the result count), `read_region` now returns `firstLine` (the UI was duplicating a private core constant), and per-workspace health (the sidebar could not say *which* workspace was degraded). |
| 2026-08-30 | **Production correctness pass.** Real Ctrl-C (was a TODO stub), multi-root watch with proper thread join, progress past 500 ms on stderr, and failures grouped by cause instead of a dim `· 156 failed`. Fixing the report exposed two more: the headline disagreed with its own detail, and it promised "findable by name" about files it had skipped — which needed the filename search branch IDX-001 requires. `tests/persistable.rs` closes the whole silent-CHECK-failure class systematically; mutation-checked. |
| 2026-08-30 | **Watcher shipped** — the last real M1 gap. `marrow watch` indexes changes live; verified by creating a file and finding it seconds later without a manual index. Hints are re-stated and re-fingerprinted before anything is believed (invariant #6); a lost event demands a sweep rather than being swallowed. |
| 2026-08-30 | **M3 started.** Tauri shell builds; five read-only commands, WebView granted no fs/shell/network (SEC-012). `marrow-query` landed. Fixed a real gap it found: **ingest never wrote `parse_results`**, so PAR-003's parser-version-driven reprocessing could not work. Then found a second bug in that fix — `format!("{:?}").to_uppercase()` gives `LOWYIELD`, not `LOW_YIELD`, so compound outcomes failed their CHECK silently into the error count while single-word ones passed on the real corpus. |
| 2026-08-30 | **M1 complete.** `search` works: 35,119 files → 13,716 parsed → 54,498 chunks in 15.6 s, queries in 0–3 ms with jumpable `path:line` and heading breadcrumbs. Three of my own bugs found by running rather than testing: a char-boundary panic in the chunker (a box-drawing glyph), the ceiling only applying to code nodes so a huge paragraph became one chunk, and the blocking `submit` reintroduced in the content stage. |
| 2026-08-30 | **End to end.** `marrow workspace add` → `index` → `status` works on the real corpus: **41,110 files in 3.6 s cold, ~1 s warm, 0 writes on re-run.** Three bugs found by running it rather than testing it: a reader connection opened per file; the blocking `submit` API (100 ms/file → ~57 min); and hardlinks fighting over `current_path` so the index never converged. |
| 2026-08-30 | **`marrow-store` done.** 36 tests. Found 6 spec defects ([D50](DECISIONS.md)); the load-bearing one: §106.1 mandates `origin_device_id`/`origin_principal_id` on every mutable row and the DDL declares neither. Settled as [D49](DECISIONS.md) and applied now — a column across 11 tables is not something to retrofit. |
| 2026-08-30 | **`marrow-scan` done.** 42 tests. Resolved M0's blocked item: 58 iCloud placeholders, 1.35 GB, 9–36 ms. Two traps found — `hidden(true)` would have hidden every placeholder (TIER-008 would read zero); APFS is normalization-*insensitive*, so NFC/NFD must be handled in our path key. D47 quantified: **34,459 files with gitignore off vs 9,435 with it on** — size M1 for ~34k. |
| 2026-08-30 | **Design pass.** GUI.md / UX.md / LLD.md written before more code. Five desktop screens mocked. D42 reversed — the desktop app is the product. |
| 2026-08-30 | **M1 started.** Workspace + `marrow-core` committed: 17 tests, clippy clean. Found ULID ordering is ms-granular not total — doc corrected, limitation pinned by a test. `store` and `scan` building in parallel. |
| 2026-08-30 | **M0 done.** Corpus is 9,435 files / 1 GB — 10× smaller than the spec assumed. Perf beats every target by 1–3 orders of magnitude. 14 PDFs total → PDF dropped from M3. Zero video/audio/email. See [bench/M0-corpus.md](bench/M0-corpus.md). |
