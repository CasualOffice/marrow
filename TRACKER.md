# Tracker

**Current milestone:** M3 — desktop shell shipped, PDFs done, **tables not started** · Part 8 S1–S5 and S7 done; S5b, S6 and S8's confirmation prompt are what is left
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
| M3 | Desktop shell + PDF + tables | `[~]` | 2026-08-30 | — |
| M4 | Semantic | `[~]` | 2026-08-30 | — |
| M5 | Write tools | `[ ]` | — | — |
| M6 | Timeline | `[ ]` | — | — |
| M7+ | Optional | `[ ]` | — | — |

**M4 was ticked here while its own section had 11 of 13 items open, and the
tick was wrong in substance too.** The vectors are built, but for a long time
the vector index had exactly **one** consumer in the repo — the desktop's Ask
path — so a 2¼-hour backfill changed nothing on any other surface. `marrow
search` gained a semantic branch in the same pass as this correction. The
desktop **Search** view (`Core::search`) and the MCP `search` tool still open
only `Fts5Index`. `[~]` until every surface that says it searches can use what
`marrow embed` built.

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
- [x] `tracing` subscriber wiring in the CLI — `init_tracing`, stderr only so
  stdout stays a clean pipe, `RUST_LOG` respected, colour off when redirected

### Discovery
- [x] Workspace + root model with explicit consent — `marrow workspace add`, and
  `add_workspace` from the desktop's Settings
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
- [x] Watch several roots at once (one thread per watcher + a shared cancel) —
  `cli/src/watching.rs`, `thread::scope`, every thread joined on shutdown
- [x] Unicode NFC/NFD normalization so macOS doesn't create duplicate identities
  — the same work as the path-identity tick above; listed twice

### Parsing — ordered by M0 file counts, not by spec order

This block was left entirely unticked long after the parsers shipped. Ticked
against `crates/parse/src/` and `ParserRouter::with_default_parsers`.

- [x] Parser trait + versioned IR with `source_span` on every node ⚠️ —
  `parser.rs`, `ir.rs`
- [~] Parser subprocess with timeout + memory cap — **not built.** `catch_unwind`
  plus `budget.rs` is the in-process half; it cannot catch a segfault in a
  Tree-sitter grammar, which is the case the subprocess exists for
- [x] Tree-sitter: Rust, TypeScript/TSX, JavaScript, Python, SQL (~1,300 files)
- [x] Plain text (449)
- [x] Markdown (289)
- [x] TOML / JSON / YAML (~165)
- [ ] Image `META` — EXIF/XMP, metadata only, never decode pixels (3,478 files,
  35% of corpus). Images are *excluded* from the text parser and terminate as
  metadata-only, so they stay findable by name (T5); **no EXIF/XMP is read**
- [x] CSV with dialect + encoding detection (~90) — delimiter sniffed unless the
  extension names one; decoding and its loss reported by `decode.rs`
- [ ] HTML / CSS (~94) — no parser registered; they fall to plain text
- [ ] XLSX / DOCX (66 — low priority, may slip to M3)
- [x] PDF — **the drop was reversed.** M0 F3 counted 14 files and dropped it;
  [D54](DECISIONS.md) built it in M3 on PDFKit, because per-character bounds are
  what make a citation to a region possible at all
- [x] Default-exclude app noise: `*.dat` `*.toc` `*.journal` `*.strings`
  `*.plist`, fonts (~2,700 files, 29%) — the exclusion list in `text.rs`
- [x] `.gitignore` respect as **per-root policy**, not global (M0 F9 — it hides
  442 xlsx) — same item as the Discovery tick above; listed twice

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
- [x] `--json` on every command; colour auto-off when piped — true again as of
  this pass. It was not: `Cmd::Embed` and `Cmd::Watch` were dispatched without
  `cli.json`, so on those two the global flag parsed and was discarded
- [~] "Why not found" explanation — the desktop has a zero-results page and the
  CLI prints a hint, but the desktop's version asserts things it has not checked
  (BUGS C6), so this is not closed
- [x] Real SIGINT handler — `cli/src/waiting.rs` installs one via `ctrlc` and
  exits 128 + SIGINT

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
- [?] Use it for a week; note which tool you actually reach for → **yours, not
  mine.** Left open deliberately rather than ticked on your behalf
- [x] `search --literal` — wired in the **CLI** first, because the zero-results
  screen had been suggesting it since M1 while the flag did not exist. It
  reports how many files it did *not* reach: "0 matches in 7,195 of 35,134
  files" plus the reason it stopped
- [x] `search_literal` over MCP — the escape hatch is now reachable from an
  agent, not only from a terminal. Carries a `coverage` block naming every file
  it did not read and why, because `matches: 0` with no coverage is how a model
  concludes a string is absent from a disk it saw a tenth of. Cloud-only files
  are skipped unread (#5) and a hit in a self-written file is returned
  `citable: false` (#9) — both mutation-verified
- [x] Each surface names its own escape hatch. `marrow search "});"` and the
  `search` MCP tool both used to answer with a library message suggesting
  `--literal` — a CLI flag, which over MCP is a suggestion that leads nowhere
  and in the desktop names nothing at all. The library message now names no
  affordance; the CLI names the flag (shell-quoted, so the printed command
  actually runs) and MCP names the tool
- [x] **The CLI and MCP could not open a real index at all** until the migration
  chain was unified. The composition roots had drifted: the CLI passed
  `fts5::MIGRATION` alone (chain to v3) while the desktop passed both (v4), so
  every `marrow search`, `marrow status` and `marrow mcp` against the app's own
  database died with `CFG_UNSUPPORTED_VERSION`. One exported
  `marrow_index::MIGRATIONS`, both roots on it, a `check.sh` guard, and
  `SCHEMA_VERSION` pinned to what the chain actually reaches
- [x] `marrow-query` landed (32 tests) — read path: RRF fusion shape, FI read
  model, explain. **Landed, not wired:** only `catalog` has callers (MCP and the
  desktop). `search_hybrid`, `intelligence` and `explain` have none; the desktop
  borrows `search::{RRF_K, LEXICAL, SEMANTIC, …}` and re-implements the fusion
  inline. ~1,900 lines exercised only by their own tests
- [x] Swapped MCP's and desktop's hand-rolled SQL onto `marrow_query::catalog`.
  Both had their own workspace listing and index status, and `roots()` was
  byte-identical in two crates — two statements answering one question about
  one index is two answers with nothing saying which is right

---

## M3 — Desktop shell + PDF + tables

**PDFs are done; tables are not started.** The milestone was scoped as one
thing and is two, and the second half is a genuine body of work rather than
a few unticked boxes.

The **desktop shell** also shipped under M3 and has no checklist here — it is
recorded only in the Log (`a8362be`, `c2141d1`, `cd9b50f`, `e883546`) and in
[BUGS.md](BUGS.md), which is where its open work now lives.

- [x] **PDFKit**, not PDFium ([D54](DECISIONS.md)) — text + page + per-character
  bbox, verified on a real 49-page document
- [x] Scanned-PDF detection (yield ≈ 0 with pages > 0) — flagged for OCR, never
  silently dropped
- [x] Provenance is `Degraded`, not `Exact`: the text is what PDFKit extracted,
  not what is on the page, and the citation badge depends on that distinction
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

### S2 — registry `[x]`
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

### S7 — creation tools `[x]`
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

**Half-built, and the missing half is the half a user sees.** The producer
works: `marrow embed` and the Models-page control fill `chunk_embeddings`. The
consumers lagged it badly — for a long time only the desktop's Ask read a
vector, so a finished backfill was invisible everywhere else. `marrow search`
now opens `SqliteVectorIndex` too. **Still lexical-only:** the desktop's
`Core::search` (the Search view) and `mcp/src/server.rs::search`, which is the
one an agent actually calls. That gap is what made the progress table's `[x]`
wrong.

- [x] Format-aware chunker with structural context prefix — landed in M1
  (structural boundaries, context prefix instead of overlap)
- [ ] Chunk-stable IDs; IR diffing so unchanged chunks keep their vectors
- [x] Embedding provider trait — `EmbeddingProvider` in `model/src/provider.rs`
- [x] Local embeddings — **neither Candle nor Ollama.** An MLX embedder in its
  own worker process ([D55](DECISIONS.md)); Ollama/LM Studio are detected if
  present but do not produce the index's vectors. [D2/D31](DECISIONS.md) is
  answered by that, and should be moved to Settled
- [x] Vector storage: brute-force cosine first — `index/src/vector.rs`,
  in-memory row cache, exact, ceiling at 1M chunks ([D1](DECISIONS.md))
- [ ] Content-addressed embedding cache
- [~] RRF fusion; weights in config, not code — the constants live in
  `query/src/search.rs` and the desktop's Ask fuses with them, but in code, not
  config, and `search_hybrid` itself has no caller
- [ ] `search --explain` — `query/src/explain.rs` exists; no CLI flag reaches it
- [x] `marrow embed` — builds semantic search over what is already indexed.
  Separate from `index` on purpose: indexing must work with no model, no GPU
  and no network, so the meaning-based half is a thing you turn on. Resumable;
  interrupting it keeps what it embedded
- [x] Semantic section on the Models page — coverage, the model, Build/Stop,
  and a measured estimate of how long the rest will take.
  `start_semantic_backfill` had existed as a command with nothing calling it
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
| 2026-08-30 | **The files that tell you what this is were describing a different project** (BUGS C21–C23, C3, C4). `CLAUDE.md` — loaded into every agent's context as instructions — opened with "Currently specification-only. No code yet." against fourteen crates, a shipped desktop app, a CLI, an MCP server and 838 tests, and told every agent not to build a UI or an agent layer when [D42](DECISIONS.md) had been reversed and [D56](DECISIONS.md) records all four agent-layer refusals as already violated. The README named Tantivy (D3 chose FTS5), PDFium (D54 chose PDFKit), Candle-or-Ollama (D55 chose an MLX sidecar) and a UI "deferred past M6". This tracker ticked **M4 Semantic done** with 11 of its 13 items open and one consumer of the vector index in the whole repo. Two more ticks were false on inspection: `--json` "on every command" (`embed` and `watch` never received it — fixed in a parallel pass, so the tick is true again) and the whole M1 parsing block, which was still unticked long after the parsers shipped — including PDF, marked *dropped* while `pdf.rs` was in the router. Two library error messages were worse than stale: `marrow reindex` does not exist and "delete the index directory" resolves to deleting `marrow.db`, corrections and all, which hard rule 8 says is the one thing that cannot be rebuilt. The pattern is the same every time: nothing enforces a document, so a document drifts silently while the code moves, and the drift is invisible until someone acts on it. |
| 2026-08-30 | **Six bugs from three screenshots, and every one was invisible to the test suite.** (1) Every answer's footer read `tokens in NaNm NaNs` — `#[serde(rename_all)]` on an enum renames the *variants*, not their fields, so every multi-word field crossed to the window in snake_case while the UI read camelCase. It silently suppressed the truncation notice too, which is part of why answers still looked like they stopped for no reason. (2) "What model are you using?" was answered *from the corpus*: retrieval found chunks containing the word "model", reported that no model name appeared in the documents, and offered "GPT-4" and "Llama-3" as examples of what it had not found — while the footer of that same answer read `qwen3.5-4b-mlx-q4`. The envelope has carried a FACT block with `trust=DETERMINISTIC_RUNTIME` since it was written and **nothing ever populated it**. (3) No text was selectable: `user-select: none` is right for chrome and wrong for an answer. (4) **No way to add a folder from the app at all** — the premise of the product needed a terminal. (5) Switching tabs destroyed the whole Ask conversation, and a generation in flight kept running with nothing to receive its tokens. (6) "Retry parsing" is a dead button aimed at a non-problem: of 46,129 parse results, **zero** are FAILED and 35,422 are METADATA_ONLY — photos and binaries with no parser, which the spec calls expected. The Status page presents that count with a warning triangle. |
| 2026-08-30 | **Dogfooding the MCP server found that the index was silently stale, and every surface reported over it confidently.** Asked marrow about its own code and got `matches: 0` for symbols that plainly exist — 109 `.rs` files on disk, 50 indexed, last scan nine hours earlier. Three compounding causes: nothing ran a watcher (`marrow watch` existed; the desktop app had no watcher code at all), `watcher_health` defaulted to `LIVE` in the schema and **nothing ever wrote it**, and `last_reconciled_at` was never written either — so a database nobody had ever watched reported a live watcher and no reader could tell a current index from a stale one. Fixed: the desktop starts one watcher per root on launch, both watchers persist health and reconciliation time to the store so *other processes* (the MCP server, the CLI) can read freshness, and `index_status` now carries `last_indexed_ms`, `watcher`, `may_be_stale` and a sentence saying what that means. A fourth bug fell out while testing: **a watcher is not listening the instant it opens, and nothing listens while the app is shut** — so a change in either window emitted no event and waited for the *six-hour* scheduled sweep. Both watchers now sweep once before listening. That is the ordinary case, not the edge one: you edit files all day with the app closed, open it, and every answer came from whenever you last ran a scan. |
| 2026-08-30 | **`search_literal` over MCP closes M2's last item of mine.** The tool that finds `});` and `TODO(name)` — the patterns FTS5 cannot express, because it tokenizes — is now callable by an agent rather than only from a terminal. The interesting part was not the scan but the payload: it reports `coverage` with `complete` and a count for every file it skipped, since a model that sees `matches: 0` and nothing else concludes the string is not on the disk, and on a 35,134-file index the scan stops on its time budget long before that is known. Fixing this surfaced a smaller one: the "no letters or digits" error came from the *library* and suggested `--literal`, a CLI flag, on every surface — over MCP that names nothing callable, and in the desktop nothing at all. The library now names no affordance and each surface names its own. |
| 2026-08-30 | **The CLI and the MCP server could not open the app's own index, and the suite was green.** Two composition roots assembled the migration chain by hand and drifted — the CLI to v3, the desktop to v4 — so `marrow search`, `marrow status` and `marrow mcp` all died with `CFG_UNSUPPORTED_VERSION` against a real database. `Store::compose` rejects a chain that is unsorted, clashing or gapped, but one that merely *stops early* is well-formed, so nothing caught it. The tests missed it because the e2e and MCP fixtures built their own partial-chain databases: they tested a shape no binary writes. Fixed with one exported `marrow_index::MIGRATIONS`, both roots and every fixture on it, and a `check.sh` guard that fails if any file assembles a chain itself. Found by running `marrow embed` against the real index — not by a test. |
| 2026-08-30 | **Semantic search was built, tested, and doing nothing.** 54,687 chunks, 0 vectors: `SemanticStatus` was in the Models snapshot and the UI never read it, and `start_semantic_backfill` was a registered command with no caller. Added `marrow embed` and the Models-page control. Measured on the real corpus: **6.4 chunks/s**, so a 35,134-file index is a ~2¼-hour build — which is exactly why the page now says so before you press the button rather than after. |
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
