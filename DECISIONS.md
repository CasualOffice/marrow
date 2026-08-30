# Decisions

Only decisions that are still live under the solo scope. The full historical log (D1–D44) is in [Part 7 §134](docs/Part_7_Solo_Rescope.md) and [Part 2 §59](docs/Part_2_Gap_Closure.md); everything commercial (D19–D30) is void.

**Convention:** a decision moves to *Settled* only when it's been acted on. Record the reason, not just the choice — future-you will want to know why.

---

## Open

### D50 — Spec defects found while implementing M1

Recorded, not yet fixed in the spec text. Each was found by building against Part 6 §106.

| # | Defect | Disposition |
|---|---|---|
| 1 | §106.1's conventions table mandates `origin_device_id` + `origin_principal_id` on every mutable row, but **no table in the DDL below it declares either** | **Fixed in code** — see [D49](#d49--sync-006-columns--origin_device_id-only-canonical-tables-only). Spec text still contradicts itself |
| 2 | §107 requires refusing a database newer than this build, but §108 has **no error code for it** | **Fixed** — added `DB_SCHEMA_TOO_NEW` to core |
| 3 | `jobs.status` defines both `FAILED` and `DEAD`, but §111.1's state machine only uses `DEAD` | M1 never writes `FAILED`. Leave the column, document the unused variant |
| 4 | `chunks.provenance_class` has no CHECK constraint while `parse_results.provenance_class` does, for the same value set | Fix when M4 writes chunks |
| 7 | **`parse_results.outcome`'s CHECK omits `METADATA_ONLY`** — the router's terminal outcome for any file with no parser (Part 3 §63 T5) | **Fixed.** Persisting a parse result for any binary would have failed the constraint; on this corpus that is all 3,478 photos. Migration v1 was edited in place rather than adding a v3 that recreates the table — forward-only matters once something real depends on a migration, and nothing does yet. Revisit that stance the moment an index exists that is worth keeping |
| 5 | No typed IDs for `file_paths.path_id`, `parse_results.parse_id`, `devices.device_id` | **Fixed** — added `PathId`, `ParseId`, `DeviceId` to core |
| 6 | A lone write blocks up to 100 ms because §50 fixes the batch at "500 rows or 100 ms" | Accepted. An adaptive early commit on an empty inbox would be faster and is exactly the "clever batching" the design warned against. Callers wanting it use `send` + `flush` |

- [ ] Amend Part 6 §106.1 to match reality (defect 1) and drop `origin_principal_id` from the convention

---

### D46 — Crate namespace on crates.io

**Needed by:** first `cargo publish`, or never if the crates stay unpublished.

The product name is settled (D45), but **`marrow` may already be taken on crates.io** — there is an Arrow-ecosystem crate by that name. This does not affect the product name, the CLI binary, the storage directory or the repo; it only affects a published crate name.

Options, in order of preference:

1. **Don't publish.** Workspace-internal crates need no unique name. A binary-only project never touches the registry. ← default
2. **Namespace the crates:** `marrow-kb-core`, `marrow-kb-cli`, or prefix with your handle
3. Publish only the binary crate under a suffixed name; keep libs private

**Action:** check `cargo search marrow` before the first publish. Until then, nothing to do.

- [ ] Verify crates.io state (only if publishing)

---

### D47 — `.gitignore` respect: global default or per-root policy? — **raised by M0**

M0 F9: `.gitignore` does 97% of the exclusion work, which is the single highest-leverage default in the system. But it also hides **442 of 475 `.xlsx` files** — real spreadsheets sitting in gitignored data directories.

FS-002 already says "where configured", so the spec permits the right answer. Make it **per-root policy**, defaulting to on for roots that look like code and off elsewhere.

**Cost now measured** (M0 F15): with gitignore **off**, `~/Desktop` yields **34,459 files** vs **9,435** with it on — 3.7×, walked in 411 ms either way. The default stands, but everything downstream in M1 should be sized for ~34k files, not 9.4k.

- [x] Implemented as per-root policy in `marrow-scan`
- [ ] Choose the per-root default heuristic (code-looking roots on, others off)

---

### D43 — Build the knowledge graph at all?

**Gate, not a date.** Build it only when you can name **three questions you actually asked** that needed entity relationships and couldn't be answered by search + timeline.

Reason: [Part 2 §55 R2](docs/Part_2_Gap_Closure.md) rates graph quality as the highest-probability, highest-impact risk in the spec. It's also the easiest subsystem to build badly, and a bad graph is worse than none — it produces confident wrong relationships you then have to un-believe.

Questions logged so far:
1. _—_
2. _—_
3. _—_

---

### D2 / D31 — Embedding and LLM runtime

Two viable paths:
- **Candle** — one crate for embeddings and generation, fewer binaries
- **Call an installed Ollama** — zero size, zero maintenance, if you already run it

Decide at M4. **Leaning Ollama-if-present** — not for disk reasons (M0's 17 GB figure was reclaimable build output; 78 GB free after cleanup), but because it is zero maintenance and already installed.

Hardware is T-mid (16 GB unified): 7–8B @ Q4 comfortable, 13–14B tight, 30B+ out of reach.

---

### D44 — Single binary or daemon split?

**Single binary + parser subprocess.** Revisit only if background indexing while the CLI is closed becomes worth it. That's when [Part 6 §110](docs/Part_6_Engineering_Reference.md) (IPC contract) stops being deferred.

---

## Settled

### D3 — Lexical index → **SQLite FTS5** *(settled at M1)*

Originally Tantivy; reopened by M0 on scale grounds (9,435 files, later revised to ~34,459 with per-root gitignore). Both are instant at that size, so scale was never going to decide it.

**The deciding argument is transactional consistency, which I under-weighted earlier.** FTS5 lives in the same database as canonical state, so an index update happens **in the same transaction** as the row it derives from. There is no window where the index and the canonical store disagree, and no dual-write to reconcile.

With Tantivy that consistency is a standing obligation: a separate directory, a separate commit, a separate crash-recovery story, and the periodic index-verification pass Part 6 §116 (V4) exists to run. All of that is real work whose only payoff at this scale is analyzer quality we would barely exercise.

| | FTS5 | Tantivy |
|---|---|---|
| Consistency with canonical state | **Free — same transaction** | A reconciliation problem |
| Dependencies added | **None** (`rusqlite` already present) | One large one |
| BM25 | Yes, via `bm25()` | Yes, richer |
| Tokenizers | `unicode61`, `porter`, `trigram` | Pluggable, better CJK |
| Crash recovery | **The database's** | Its own |
| Speed at 34k docs | Instant | Instant |

Tantivy wins on analyzer quality and at a scale this corpus will not reach. It stays reachable: the `TextIndex` port ([LLD §2.1](docs/LLD.md)) exists precisely so this is one adapter, and Part 2 §36.3 already sanctions the FTS5-only profile.

**Revisit if** CJK content appears in quantity, or field-weighted BM25 measurably beats what FTS5 gives on the golden query set.

#### Measured after implementing — 34,459 docs, release, M-series

Latency is a function of **document frequency**, not corpus size, so the benchmark corpus is Zipf-shaped with planted terms of known df. A uniform vocabulary would make every term a worst case and tell you nothing.

| Query | p50 | p95 |
|---|---|---|
| unique term (df=1) | **30 µs** | 33 µs |
| df 0.1% | 187 µs | 567 µs |
| df 10% | 3.9 ms | 6.0 ms |
| df 100% | **11.8 ms** | 15.2 ms |
| prefix, as-you-type | **415 µs** | — |
| phrase | 214 µs | — |
| df 10% + path glob | 3.5 ms | — |
| full rebuild | 6.0 s | — |

LLD §8's ~5 ms lexical budget holds to ~10% df, and as-you-type is 0.4 ms — inside GUI §5.2's 8 ms first-paint.

#### Two things the D3 reasoning got wrong

| # | Correction |
|---|---|
| 1 | **BM25 has no early termination.** `LIMIT 100` does not stop FTS5 scoring postings — a term in every document costs 11.8 ms because all 34,459 postings are scored, then sorted. "Both are instant at that size" is true for realistic df, not for a stopword-like term. The `ORDER BY rank … rank MATCH 'bm25(…)'` form the docs imply is optimized measured **slower** (36 ms vs 24 ms), so the plain `bm25()` alias stands. If 100%-df ever matters the fix is a stopword list or a df cap in `query`, not different SQL |
| 2 | **The storage cost was not priced.** FTS5 must be self-contained for `snippet()`/`highlight()` to work — a contentless table cannot produce them — so chunk text is stored **twice**, plus prefix indexes. **136 MB for 34k documents.** The comparison table said "dependencies added: none" and said nothing about the database roughly doubling. Tantivy's separate directory would have been comparable, so the decision does not change, but the table was incomplete |

#### Migration composition

`marrow-index` depends on `marrow-store`, so store cannot reference index's migration back — that is a cycle. The **composition root** (the binary) assembles the chain instead, via `Store::open_with_migrations`. Store stays unaware of index; index stays a swappable implementation of a port. Verified: a fresh database opens at `schema_version 2` with both migrations applied through the one runner, backup-before-migrate intact.

### D48 — MCP before the desktop shell? → **MCP first, and it was the right call** *(settled 2026-08-30)*

MCP shipped first and the query API was validated through it before three
panes were built on it, which is exactly the argument that won. The risk it
was weighed against — S7, interest fading before something is usable daily —
did not materialise.

What did happen is [D56](#d56--the-milestone-boundary-dissolved-and-nobody-said-so).

### D54 — PDF text and geometry → **PDFKit, not PDFium** *(settled 2026-08-30)*

M3 planned PDFium. PDFKit wins on the thing that actually matters:
`characterBoundsAtIndex` returns a rectangle **per character** in page
coordinates, which is what makes a citation to an exact region possible rather
than a citation to a page number. Verified on a real 49-page document before
any of it was built on: page count, media box and per-character rects all
present.

It also removes a multi-megabyte Chromium library to vendor, sign, notarize and
version-track, in exchange for a framework already on every Mac.

The cost, stated: it is macOS-only. Elsewhere the parser refuses and the file
stays findable by name (T5), which is the same outcome as having no PDF parser
at all — so this trades a portability we do not have for a capability we do.

The trap it introduced is worth remembering: `characterBoundsAtIndex` indexes an
`NSString`, which is UTF-16, while Rust offsets are UTF-8. They agree on ASCII
and diverge on everything else, so the naive version is correct on the documents
you test with and wrong on the ones with an em dash in them.

### D55 — Replace the Python MLX sidecar? → **No. Keep it, bundle the venv** *(settled 2026-08-30)*

A comparison suggested `omlx` had already built Part 8's supervisor and that
three teams had written real Rust MLX bindings. Investigated properly
([Decision_MLX_Runtime.md](docs/Decision_MLX_Runtime.md)):

- `omlx` is real and its supervisor is good, but it publishes **no library** —
  only an HTTP service — and depends on the same `mlx-lm` the sidecar does,
  pinned harder.
- Three teams wrote Rust bindings; **none published one**. The only published
  binding has generation commented out on main and an empty KV cache.
- Routing embeddings through omlx would reintroduce the padding defect
  `_embed` was written to avoid, because its API has no `padding` field and it
  length-sorts inputs. A chunk's vector would depend on what was batched with
  it, so a rebuild would not reproduce the index.

For one person on one Mac who wants this working in two years, an ugly sidecar
that works beats a dependency that is unmaintained in eighteen months.

The real fragility is not the sidecar: `mlx` and `mlx-lm` are Apple's and
pushed daily, but **`mlx-embeddings` is one person's**. That is the thing to
watch.

### D56 — The milestone boundary dissolved, and nobody said so *(recorded 2026-08-30)*

Recorded because it is a process decision that was made by not making it.

[D-agent-layer](#d-agent-layer--build-our-own-agent-runtime-model-gateway-approval-ux-chat-ui--no)
says: no agent runtime, no model gateway, no approval UX, no chat UI — Marrow
is the index, MCP is the interface, and that deletes ~60% of the spec from the
critical path.

All four now exist. A desktop app, a model supervisor with admission and a
circuit breaker, a conversational Ask surface with streaming and citations.
Each was asked for and each was built, and none of them was weighed against
D-agent-layer at the time. Part 7 names this as risk **S1**; it is no longer
hypothetical.

Two things follow, and only the second is a decision:

1. **D-agent-layer is superseded in fact.** The reversal of [D42](#d42--build-a-gui--yes-the-desktop-app-is-the-product-reversed-2026-08-30)
   already made the desktop app the product, and a desktop app that answers
   questions needs a model gateway. That is coherent. What was missing is that
   it was never written down, so the constraint stayed in the file saying the
   opposite of what was being built.
2. **The rule that failed was "check the tracker before building".** It did not
   fail because it was wrong; it failed because a long run of *proceed* is
   exactly the condition under which nobody re-reads it. There is no mechanism
   proposed here — a solo project cannot police itself with process — but the
   drift is now on the record, which is the only thing that makes the next one
   visible.

### D49 — SYNC-006 columns → **`origin_device_id` only, canonical tables only**

Part 6 §106.1 mandates `origin_device_id` **and** `origin_principal_id` on every mutable row. The DDL declares neither on any M1 table. Settled during M1 rather than deferred, because adding a column to eleven tables later means a migration plus a backfill that cannot know the answer.

**Keep `origin_device_id`.** Part 7 §132 explicitly retains it in the reduced SYNC set as cheap future-proofing — it is what makes a future multi-device merge possible at all. Nullable, unused on one machine, free.

**Drop `origin_principal_id`.** Part 7 §124 reduced MULTI to three requirements and dropped per-principal tracking, so a `principals` table never arrives. A foreign key to a table that will not exist is not future-proofing, it is clutter.

Applied to the four **canonical, non-rebuildable** tables: `workspaces`, `workspace_roots`, `files`, `file_versions`. Derived tables (`parse_results`, `ir_nodes`, `chunks`, `jobs`, `file_paths`) are regenerated per device and carry no origin.

### D51 — Cloud placeholder hydration → **Marrow triggers the system's own hydration**

Decided 2026-08-30. Marrow calls the macOS API Finder uses, so the user stays in one app. Still explicit, still shows the size first, never automatic.

Consequence: TIER-006 (per-workspace opt-in with size shown), TIER-007 (rate-limited and cancellable), TIER-010 (suspend on metered connection) and TIER-011 (never on battery without override) all become real M1+ work rather than being satisfied by "we never hydrate".

**TIER-005 still holds absolutely for the indexer** — the scan path never hydrates. Hydration is a separate, user-initiated action with its own progress and cancel. The two are not allowed to touch.

Measured cost for this machine: 58 files, 1.35 GB (M0 §9).

### D1 — Vector store → **brute-force cosine, indefinitely** *(settled by M0)*

Corpus is 9,435 files ≈ 30–60k chunks. Cosine over 60k × 384 floats is single-digit milliseconds in release mode. LanceDB would be a dependency, a storage format, a generation-migration mechanism and a failure mode, all serving nothing measurable.

Revisit only if the corpus grows past ~500k chunks, which on this evidence it will not.

### D4 — PDF engine → **deferred indefinitely** *(settled by M0)*

**14 PDF files in the entire home directory.** PDFium plus page/bbox provenance, scanned-PDF detection, OCR routing and borderless-table reconstruction is roughly 15 weeks of spec'd work serving fourteen files.

Dropped from M3. If the corpus ever changes, this reverses cheaply — the parser tier model (Part 3 §63) already has a slot for it.

### D5 — Platform → **macOS 26.3, Apple Silicon (M4-class, 16 GB, 10 cores)** *(recorded by M0)*

Consequences: FSEvents watcher semantics with event-ID replay · native Vision framework for OCR if ever needed (0 MB) · NFD filename normalization is mandatory, not optional · `SF_DATALESS` / `.icloud` stubs are the placeholder detection path.

Disk was briefly a constraint at 17 GB free; 64 GB of that was reclaimable Rust `target/` output. Now 78 GB free — not a constraint.

### D42 — Build a GUI? → **Yes. The desktop app is the product** *(reversed 2026-08-30)*

Previously deferred past M6 on the Part 7 §130 argument that Claude Code already provides a front-end, so a UI would be rebuilding what exists.

**Reversed by the author.** The desktop app is the product surface.

What this reverses: Part 7 §130's UI deletion, and Part 1 §16's information architecture returns (trimmed to what ships — see [GUI.md](docs/GUI.md) §4).

What it does **not** reverse — §130's other half still stands: no agent runtime, no model gateway, no approval-UX-as-chat. A GUI does not require owning inference. `Ask` is a query surface with citations, not a conversation with tool-calling.

Stack: Tauri 2 + React + TypeScript (Part 1 §17.1). Three frontends — desktop, CLI, MCP — over one core, which strengthens the ports-and-adapters seam rather than complicating it.

### D45 — Product name → **Marrow**

Decided 2026-08-30.

The dense substance deep inside — what you get to when you cut through. It fits what the thing does: strip away formatting and surface text, keep the structure and the evidence underneath. Distinctive, six letters, and it doesn't read as an AI product.

Considered and passed over: **Cairn** (markers you stack yourself — closest runner-up), **Strata** (layers of evidence over time), **Quarry** (extraction + "to search diligently"), Tessera, Lodestone, Fathom, Loam.

Applied: docs, scaffold, storage dir (`~/.local/share/marrow/`), planned crates (`marrow-core`, `marrow-cli`, `marrow-mcp`), daemon name (`marrowd`, if it ever splits). Spec files renamed `Part_N_*.md`. See [D46](#d46--crate-namespace-on-cratesio) for the one open loose end.

### D41 — Project licence → **Apache-2.0**

Keeps every future door open, including relicensing your own code, and grants patent rights MIT doesn't. GPL dependencies remain usable when invoked as **separate processes** (ffmpeg via CLI, a Python sidecar) — only linking creates an obligation. See [Part 7 §128](docs/Part_7_Solo_Rescope.md).

### D33 — Execution tier E2 → **merged into E4**

Not worth a separate tier for a single operator. See [Part 7 §129](docs/Part_7_Solo_Rescope.md).

### D58 — May a surface report index counts without reporting freshness? → **No** *(settled 2026-08-30)*

A stale index is worse than no index. No index answers nothing and the user knows to scan; a stale one answers confidently about a disk it has not looked at, and nothing in the result says so. The failure is silent by construction — the counts are *true*, they are just true about a disk that has moved on.

This shipped in every surface at once. `index_status` over MCP reported 35,134 files with no timestamp; the desktop's status page showed five real numbers per workspace and no freshness; `watcher_health` defaulted to `LIVE` in the schema and nothing ever wrote it, so a database nobody had ever watched reported a live watcher; `last_reconciled_at` was never written at all. Asking Marrow about its own source returned `matches: 0` for symbols that plainly existed, because half the files had never been scanned.

Two rules follow. **Freshness is persisted, not held in memory** — the MCP server and the CLI are separate short-lived processes, so freshness that lives only in the desktop app cannot be reported by the surface an agent actually calls. And **the honest default is `unavailable`**: a root that has never been reconciled is treated as unwatched whatever the column says, because a default that reads `LIVE` is a lie every reader downstream repeats.

The corollary is that the app has to *earn* the fresh state: the desktop now runs a watcher per root, and both watchers sweep once before they start listening. A watcher is not live the instant it opens, and nothing is listening while the app is shut — so without that first sweep a change in either window waits six hours for the scheduled reconciliation, which is indistinguishable from having no watcher at all.

### D59 — Context window, structured output and the 4B ceiling *(settled 2026-08-30)*

Prompted by a review of the model layer. Four of the five points hold; two land differently in this stack than they would in llama.cpp, and one was already validated by our own measurements.

**Never run at the advertised ceiling.** Qwen 3.5 4B reports a 262,144-token context and we run at 8,192. KV is ~160 KB per token — an architectural property, not a function of parameter count — so 8k costs about 1.3 GB and 16k costs 2.6 GB against weights of ~2.5 GB. The ceiling is displayed beside the run context precisely so the gap is visible. The subtlety the review is right about: `default_context` was a *planning* number for sizing the memory watchdog, not an enforced limit, because MLX allocates KV lazily. Enforcement is now real and lives in two places — the evidence is bounded to 16 KB before assembly, and the answer budget asks how much fits in the memory that is free rather than subtracting the prompt from a constant.

**Constrained decoding: not applicable yet, and the reason matters.** Nothing in this system asks the 4B for JSON or for a tool call. The MCP tools are called by an external agent; the local model only answers from evidence. So there is no format to drift on today. It becomes load-bearing the moment the intent router lands, and `mlx-lm` accepts `logits_processors`, so schema-constrained sampling is available in this stack — the llama.cpp GBNF answer has an MLX equivalent. **The router must not ship without it.** Retrying malformed output is a worse design than making it structurally impossible, and discovering that after the router is built is the expensive order.

**Tiering already exists** — `Profile::{Efficient, Balanced, LargerLocal, Cloud}` chosen from probed memory, with `LargerLocal` covering 8B and up where it fits. A 32 GB machine still defaults to Balanced/4B, which is a deliberate choice about battery and thermals rather than an oversight.

**CPU-only is out of scope, and that is a decision, not an omission.** This is MLX on Apple Silicon (D55); there is no CPU path to be slow on. It also means we have excluded those users entirely, which is worth stating plainly rather than discovering later.

**The GGUF quantisation warning does not apply, but its underlying point already bit us.** We do not use llama.cpp or GGUF at all. But the warning — a new architecture behaves differently from what tooling assumes — was correct, and we hit it somewhere else: Qwen 3.5's hybrid cache mixes `ArraysCache` with `KVCache`, `can_trim_prompt_cache` returns false for it, and KV prefix reuse therefore does not work on this model at all. The 81% reuse figure applies to the 0.6B. The worker reports `cacheTrimmable` for exactly this reason. So: test the actual artefact, confirmed — and the fallbacks named (Ministral 3 3B, Gemma 4 E4B) are worth keeping in the catalogue's sights.

### D57 — Who assembles the migration chain? → **`marrow_index::MIGRATIONS`, never a composition root** *(settled 2026-08-30)*

The chain is numbered across crates: `marrow-store` owns 1 and 3, `marrow-index` owns 2 and 4, and `marrow-index` depends on `marrow-store` so store cannot reference it back ([D3](#d3)). The binary composing them is right. The binary *enumerating* them is not.

`Store::compose` validates thoroughly — it rejects an unsorted chain, two migrations claiming one version, and a gap. It cannot reject a chain that merely stops early, because `[1, 2, 3]` is a well-formed chain. So a root that names a subset of the extensions compiles, migrates cleanly, passes its tests, and then refuses to open the database the other root wrote.

That shipped. The CLI passed `fts5::MIGRATION` alone and the desktop passed both, and every `marrow search`, `marrow status` and `marrow mcp` against a real index failed with `CFG_UNSUPPORTED_VERSION` — the MCP server, which is the whole M2 deliverable, was dead on the author's own machine. The suite stayed green because the e2e and MCP fixtures also built partial-chain databases: **they tested a schema no binary writes.**

The rule: a crate that contributes migrations exports the complete list; composition roots pass that constant and nothing else; fixtures use it too, so a test database is the shape a real one is. `check.sh` fails if any file outside `crates/index/src/lib.rs` names an individual migration. Adding a migration is then one edit that reaches every binary.

### D-sandbox — Build an OS sandbox? → **No, never**

A sandbox protects unknown users from a malicious model. You are the operator and run arbitrary shell all day. The reference implementation is reportedly ~17k lines. Structured argv and env allowlists stay — as bug prevention, not security controls.

### D-agent-layer — Build our own agent runtime, model gateway, approval UX, chat UI? → **No**

Claude Code / Codex / Cursor already do this well and are on the machine. Marrow is the index; MCP is the interface. This deletes roughly 60% of the spec from the critical path. See [Part 7 §130](docs/Part_7_Solo_Rescope.md).

### D13 — OCR engine → **platform-native**

Free (0 MB) and better than bundled Tesseract on macOS and Windows. Tesseract only on Linux.

### D17 — GPS/location extraction → **off by default**

It's your own photo library; turn it on deliberately if you want it.

### D37 — Recipe format → **public, plain JSON**

You will hand-edit it. Don't invent a DSL.

---

## Void under solo scope

D10, D11 (business model, positioning), D19–D30 (commercial, compliance, distribution), D22 (managed inference), D25 (app stores). Retained in [Part 4](docs/Part_4_Commercial_Superseded.md) in case the project's scope changes.

---

## How to add one

```markdown
### Dnn — Short question

**Needed by:** milestone or trigger
**Options:** …
**Leaning:** … because …
**Decided:** date → choice, and the reason it won
```
