# Marrow — Local Knowledge & Agent Runtime

A local knowledge runtime: continuously indexes folders you point it at, understands their structure, answers questions with citations to the exact page / cell / line, and exposes all of it to whatever agent front-end you already use.

**Status of *this directory*:** the specification — the design as it was written. **Status of the project:** built and used daily — fourteen crates, a Tauri desktop app, a `marrow` CLI, an MCP server over stdio, 1,100+ tests. M0–M2 are done, M3 and M4 are in progress. See [TRACKER.md](../TRACKER.md) for the real state and [BUGS.md](../BUGS.md) for what is currently wrong.
**Scope:** Personal project, single user, single machine, open source. **Not a product.**

> **The spec is the design; the tracker is the state.** The spec still runs ahead of the build in most places, and in a handful of places the build went the other way. **Never infer from a section here that something is implemented.**

> **Read [Part 7](Part_7_Solo_Rescope.md) first.** Parts 1–6 were written for a commercial multi-user product. Part 7 re-scopes everything for solo self-use and **supersedes Part 4 entirely**. Reading Parts 1–6 without it will send you building the wrong thing.

> **And [DECISIONS.md](../DECISIONS.md) supersedes every part.** Where building it proved a part wrong, the decision is the answer and the spec text was deliberately left alone — the parts record the plan as it was made, and annotating rather than rewriting is how supersession is recorded here. Check DECISIONS before citing any section.

---

## The thesis

Not "an MCP filesystem server with embeddings," and not "a chatbot with permission to read files." The durable asset is the **provenance-backed knowledge representation** — files, structure, history, entities, evidence, corrections. The model is replaceable; the knowledge is not.

Two invariants carry the design: **deterministic before probabilistic**, and **policy below the model** — retrieved content is data, never authority.

A third was written for solo use and **did not survive the build**: ~~don't rebuild the agent layer~~ — Claude Code already does that well, so build the index and expose it over MCP. Then [D42](../DECISIONS.md) made the desktop app the product, and an app that answers questions needs a model runtime; a supervisor, a local generator and a conversational Ask surface all exist now ([D56](../DECISIONS.md)). MCP is still first-class, and the index is still the durable asset — the agent layer is not.

---

## Reading order

| Part | File | Covers | Solo relevance |
|---|---|---|---|
| **7** | [Part 7 — Solo Re-Scope](Part_7_Solo_Rescope.md) | §123–135. What's cut (~140 reqs), what must not be cut (14 items), MCP-first inversion, revised build plan in weeks | **Start here.** Supersedes Part 4 |
| **1** | [Master Specification](Part_1_Master_Specification.md) | §1–43. Vision, requirements, trust model, architecture, data model, ingestion, retrieval, transactions, ADRs, threat model | **Core.** Read §1, §6, §10, §12 closely |
| **2** | [Part 2 — Gap Closure](Part_2_Gap_Closure.md) | §44–60. Cloud placeholders, watcher limits, verification subsystem, reversibility, Tier C governor, SQLite writes, honest sandbox posture | **Core.** §45.1, §46, §47, §50 all still apply |
| **3** | [Part 3 — Conversion & Multimodal](Part_3_Conversion_Multimodal.md) | §61–76. Parser tiers, OCR, images, video, audio, embedded metadata | **Partial.** §61–63 and §69 matter; video/audio deferred |
| **4** | [Part 4 — Commercial & Compliance](Part_4_Commercial_Superseded.md) | §77–92. SKUs, pricing, GTM, support, compliance, DPAs | ~~**Superseded by Part 7.**~~ Parked in case that changes |
| **5** | [Part 5 — Capabilities](Part_5_Capabilities.md) | §93–104. Prior art, hardware probe, local LLM, execution tiers, generative media, file intelligence, tables, agent parity, answer coverage | **Core**, as amended by Part 7 §129 (execution) and §127 (hardware) |
| **6** | [Part 6 — Engineering Reference](Part_6_Engineering_Reference.md) | §105–122. SQLite DDL, migrations, errors, config, IPC, jobs, chunking, fusion, context envelope, tests, budgets, glossary, indexes | **Build from this.** Trim the DDL per Part 7 §135.1; §110 IPC is deferred |
| **8** | [Part 8 — Model Runtime](Part_8_Model_Runtime.md) | §136–150. Hardware probe and live sampling, model registry and recommendation, dynamic loading, **the supervisor** (admission, circuit breaker, queues, model workspace), cloud providers, loading states | **Core.** The foundation for semantic search, Ask and graph extraction alike. Written after Part 7 and supersedes it here |
| **9** | [Part 9 — Egress](Part_9_Egress.md) | §151+. What may leave the machine, the `fetch` tool, and why the policy is written before the implementation | **Core.** Written after Part 7 and supersedes it here |

### Design documents

Written after the spec, and they win over it for the surfaces they cover.

| Doc | Covers |
|---|---|
| [GUI.md](GUI.md) | **Desktop app** — stack, Tauri boundary, IA, the two modes, interaction, visual direction, performance budgets. Reverses D42; supersedes Part 1 §16 |
| [UX.md](UX.md) | **Terminal + MCP** — command surface, result rendering, errors, machine output |
| [LLD.md](LLD.md) | **Internals** — layering, patterns used *and rejected*, concurrency, error strategy, seams, testing |
| [Comparison.md](Comparison.md) | **Prior art on the safety axes** — provenance, injection, write tools, egress, self-citation |
| [RESEARCH_LANDSCAPE.md](RESEARCH_LANDSCAPE.md) | **Prior art on the product axis** — what eight chat/RAG/agent products are like to use |
| [Decision_MLX_Runtime.md](Decision_MLX_Runtime.md) | **Why the model runtime is a bundled MLX sidecar** — the long form behind [D55](../DECISIONS.md) |

Both design docs written before the desktop app ([UX.md](UX.md), [LLD.md](LLD.md)) still carry the pre-reversal framing in places — UX.md's header calls the GUI deferred, LLD.md treats D3 as open. [GUI.md](GUI.md) and [DECISIONS.md](../DECISIONS.md) are the current word on both.

**Conflict rule:** later parts supersede earlier ones, the design docs supersede the spec for their surface, and **[DECISIONS.md](../DECISIONS.md) supersedes all of them**. Part 7 wins over Parts 1–6; Parts 8 and 9 win over Part 7 in their own areas.

---

## Twenty-minute version

| Read | Why |
|---|---|
| §123, §126, §130 (Part 7) | The re-scope, the 14 non-negotiables, and the MCP-first inversion |
| §131 (Part 7) | The actual build plan |
| §1.2 (Part 1) | Ten design principles the whole thing rests on |
| §6.2 + ADR-007 | Why file content never grants authority |
| §61 (Part 3) | Why provenance is the point, in one table |
| §46, §47 (Part 2) | Verification and reversibility — what separates this from a demo |
| §98.4 (Part 5) | Self-poisoning: the subtlest failure mode here |

---

## Build plan (Part 7 §131)

The plan as written, with where it actually got to. **[TRACKER.md](../TRACKER.md) is authoritative** — this column goes stale the moment someone commits.

| M | Milestone | Part-time | Where it got to |
|---|---|---|---|
| M0 | Measure your own corpus + walk/hash/store spike | 1 wk | Done |
| M1 | Scan, watch, reconcile, SQLite, text/md/code/CSV parsers, ~~Tantivy~~ **SQLite FTS5** ([D3](../DECISIONS.md)), CLI search | 6–10 wk | Done |
| **M2** | **MCP server — search, read, file intelligence** | **1–2 wk** | Done — ten tools over stdio |
| M3 | PDF text + page provenance; native table IR + compute | 4–7 wk | In progress. PDF text done on PDFKit ([D54](../DECISIONS.md)); tables read from CSV/MD/HTML/XLSX/DOCX; PDF *ruled* tables and `table compute` not built |
| M4 | Chunking, local embeddings, vector search, RRF hybrid | 4–6 wk | In progress. Vectors reach the desktop's Ask and `marrow search --semantic`, not the desktop Search view or the MCP `search` tool |
| M5 | Write tools: patch, stale-check, snapshots, undo, E1 recipes | 4–6 wk | Not started |
| M6 | Timeline + Git integration | 3–4 wk | Not started |
| M7+ | Graph, OCR, local LLM, UI — only if you miss them | open | **Three of the four arrived early.** OCR ([D60](../DECISIONS.md)), the local model ([Part 8](Part_8_Model_Runtime.md), [D55](../DECISIONS.md)) and the UI ([D42](../DECISIONS.md), reversed) all shipped before M5. The graph is still refused ([D43](../DECISIONS.md)) |

**M2 was the forcing function**, and it worked: MCP shipped before the desktop shell and validated the query API first ([D48](../DECISIONS.md)). What the plan did not anticipate is that M7's optional items would be pulled forward past M5 and M6 — recorded, not tidied away, as [D56](../DECISIONS.md).

---

## Non-negotiables

Cheap now, expensive or impossible later:

- **Path ≠ identity** — stable file IDs + path history
- **`source_span` from day one** — provenance is the entire reason to build this
- Provenance + authority class on every derived fact
- Idempotent, resumable jobs; indexes rebuildable from canonical state
- **Cloud placeholders never silently hydrated** (TIER-005)
- Untrusted-content boundary — file text can never grant tool authority
- Path canonicalization + symlink escape checks
- Stale-version check before any write; snapshots + undo
- Backup before migration — derived data is disposable, your corrections aren't
- Search works with no LLM, no GPU, no network
- Unicode NFC/NFD normalization (a correctness bug, not a locale feature)

## Non-goals

Whole-OS indexing without consent · autonomous destructive actions · screen recording · face recognition · voice ID · multi-device sync · mobile/web · replacing Git or filesystem ACLs · treating embeddings as canonical truth · **an OS sandbox** (Part 7 §129 — settled permanently).

One entry on this list **did not hold**: ~~its own chat UI or agent loop (Part 7 §130)~~. A desktop app, a model supervisor with admission and a circuit breaker, and a conversational Ask surface with streaming citations all exist. [D42](../DECISIONS.md) made the desktop app the product, and [D56](../DECISIONS.md) records the rest as superseded *in fact* rather than repealed by argument. That is a warning about how the boundary dissolved, not a licence to build more of it — §130 is still the reason to keep the agent layer small.

---

## Stack

What is actually built, and what each choice replaced. Four of the original
picks were reversed while building; each names the decision that reversed it
rather than pretending the spec said this all along.

```text
Language     Rust · Tokio · serde · tracing
Filesystem   ignore · notify · blake3 · globset
Canonical    SQLite (WAL, single-writer actor)
Full text    SQLite FTS5 — same transaction as canonical state (D3, not Tantivy)
Vector       brute-force cosine over SQLite, indefinitely (D1); revisit past ~500k chunks
Parsers      Tree-sitter (code) · PDFKit (PDF — D54, not PDFium) · Vision for image
             text (D60) · calamine (XLSX) · zipped-XML reader (DOCX) · tag scanner
             (HTML) — all in-process; the isolating subprocess is not built
Embeddings   MLX in a bundled worker process (D55, not Candle or Ollama);
             an installed Ollama / LM Studio is detected if present (D2/D31 open)
Generation   MLX worker under a supervisor with admission, a breaker and a KV cache
             (Part 8) · any OpenAI-compatible endpoint behind the same trait (§140)
Interface    Desktop app · CLI · MCP server over stdio
Front-end    Claude Code / Codex / Cursor — still first-class over MCP
UI           Tauri 2 + React + TypeScript — the desktop app is the product
             (D42, reversed; ~~deferred past M6~~). GUI.md §4 bounds what ships
Platform     macOS on Apple Silicon (D5). Windows and Linux do not work yet
Licence      Apache-2.0 (D41)
```

---

## Facts

| | |
|---|---|
| Requirements | ~245 after the Part 7 trim (was ~520); ~120 in M1–M4 |
| Open decisions | D2/D31, D43, D44, D46, D47, D50. Part 7 §134's list (D1–D5, D13, D17, D37, D41–D44) is the *original* one — most of it has since settled; [DECISIONS.md](../DECISIONS.md) is the live split |
| Top risks | **S1 scope collapse** — no longer hypothetical, see [D56](../DECISIONS.md) · S2 building the graph too early · S3 injection reaching a write tool · ~~S7 interest fading before M2~~ (M2 shipped) |
| ADRs | ADR-001…013 |
| Hardware target | Narrowed by [D5](../DECISIONS.md) from "16–32 GB, modern CPU, optional GPU" to **one reference machine: Apple Silicon, 16 GB**. No degradation tiers — if it does not run there, that is a bug |
| Corpus target | **Your actual corpus** — measure it before assuming the spec's 100k files |

---

## Conventions

| | |
|---|---|
| `PREFIX-NNN` | Requirement ID. Permanent, never reused |
| `§N` | Section number, continuous across all nine parts |
| `Dn` / `Rn` / `Sn` / `ADR-n` | Decision / risk / solo risk / architecture decision |
| **[ASSUMPTION]** / **[COUNSEL]** | Part 4 only — both moot under Part 7 |

Numbering is append-only; superseded sections are annotated, not renumbered — **the parts are never rewritten to match what got built.** That is the mechanism: the spec records the plan, [DECISIONS.md](../DECISIONS.md) records where the plan turned out wrong, and the two are read together. Drift between Part 6 §106–119 and the code is a defect in the docs.

**Warning, carried in the [project README](../README.md):** this indexes files you point it at. Don't point it at anything you wouldn't want an LLM to read.
