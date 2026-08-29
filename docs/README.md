# LKAR — Local Knowledge & Agent Runtime

A local knowledge runtime: continuously indexes folders you point it at, understands their structure, answers questions with citations to the exact page / cell / line, and exposes all of it to whatever agent front-end you already use.

**Status:** Specification only — no implementation yet.
**Scope:** Personal project, single user, single machine, open source. **Not a product.**

> **Read [Part 7](LKAR_Addendum_Part_7.md) first.** Parts 1–6 were written for a commercial multi-user product. Part 7 re-scopes everything for solo self-use and **supersedes Part 4 entirely**. Reading Parts 1–6 without it will send you building the wrong thing.

---

## The thesis

Not "an MCP filesystem server with embeddings," and not "a chatbot with permission to read files." The durable asset is the **provenance-backed knowledge representation** — files, structure, history, entities, evidence, corrections. The model is replaceable; the knowledge is not.

Two invariants carry the design: **deterministic before probabilistic**, and **policy below the model** — retrieved content is data, never authority.

For solo use, one more: **don't rebuild the agent layer.** Claude Code and friends already do that well. Build the index and expose it over MCP.

---

## Reading order

| Part | File | Covers | Solo relevance |
|---|---|---|---|
| **7** | [Part 7 — Solo Re-Scope](LKAR_Addendum_Part_7.md) | §123–135. What's cut (~140 reqs), what must not be cut (14 items), MCP-first inversion, revised build plan in weeks | **Start here.** Supersedes Part 4 |
| **1** | [Master Specification](Local_Knowledge_Agent_Runtime_Master_Specification.md) | §1–43. Vision, requirements, trust model, architecture, data model, ingestion, retrieval, transactions, ADRs, threat model | **Core.** Read §1, §6, §10, §12 closely |
| **2** | [Part 2 — Gap Closure](LKAR_Addendum_Part_2.md) | §44–60. Cloud placeholders, watcher limits, verification subsystem, reversibility, Tier C governor, SQLite writes, honest sandbox posture | **Core.** §45.1, §46, §47, §50 all still apply |
| **3** | [Part 3 — Conversion & Multimodal](LKAR_Addendum_Part_3.md) | §61–76. Parser tiers, OCR, images, video, audio, embedded metadata | **Partial.** §61–63 and §69 matter; video/audio deferred |
| **4** | [Part 4 — Commercial & Compliance](LKAR_Addendum_Part_4.md) | §77–92. SKUs, pricing, GTM, support, compliance, DPAs | ~~**Superseded by Part 7.**~~ Parked in case that changes |
| **5** | [Part 5 — Capabilities](LKAR_Addendum_Part_5.md) | §93–104. Prior art, hardware probe, local LLM, execution tiers, generative media, file intelligence, tables, agent parity, answer coverage | **Core**, as amended by Part 7 §129 (execution) and §127 (hardware) |
| **6** | [Part 6 — Engineering Reference](LKAR_Addendum_Part_6.md) | §105–122. SQLite DDL, migrations, errors, config, IPC, jobs, chunking, fusion, context envelope, tests, budgets, glossary, indexes | **Build from this.** Trim the DDL per Part 7 §135.1; §110 IPC is deferred |

**Conflict rule:** later parts supersede earlier ones. Part 7 wins over everything.

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

| M | Milestone | Part-time | Done when |
|---|---|---|---|
| M0 | Measure your own corpus + walk/hash/store spike | 1 wk | You know your real numbers |
| M1 | Scan, watch, reconcile, SQLite, text/md/code/CSV parsers, Tantivy, CLI search | 6–10 wk | You use it instead of grep |
| **M2** | **MCP server — search, read, file intelligence** | **1–2 wk** | **Your coding agent can search everything you own** |
| M3 | PDF text + page provenance; native table IR + compute | 4–7 wk | Cited numbers from spreadsheets |
| M4 | Chunking, local embeddings, vector search, RRF hybrid | 4–6 wk | Semantic beats lexical on your queries |
| M5 | Write tools: patch, stale-check, snapshots, undo, E1 recipes | 4–6 wk | You let an agent edit *through* LKAR |
| M6 | Timeline + Git integration | 3–4 wk | You can reconstruct last month |
| M7+ | Graph, OCR, local LLM, UI — only if you miss them | open | — |

**M2 is the forcing function.** 7–12 weeks part-time to daily usefulness. Everything after it is informed by real use.

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

Whole-OS indexing without consent · autonomous destructive actions · screen recording · face recognition · voice ID · multi-device sync · mobile/web · replacing Git or filesystem ACLs · treating embeddings as canonical truth · **an OS sandbox** (Part 7 §129) · **its own chat UI or agent loop** (Part 7 §130).

---

## Stack

```text
Language     Rust · Tokio · serde · tracing
Filesystem   ignore · notify · blake3 · globset
Canonical    SQLite (WAL, single-writer actor)
Full text    Tantivy
Vector       brute-force cosine at M4 → LanceDB only when it hurts (D1)
Parsers      Tree-sitter (code) · PDFium (PDF) · calamine (XLSX) — in a subprocess
Embeddings   Candle, or call an already-installed Ollama (D2/D31)
Interface    CLI + MCP server over stdio   ← the primary UI
Front-end    Claude Code / Codex / Cursor  ← already exists, don't rebuild it
UI           Deferred past M6 (D42)
Licence      Apache-2.0 (D41)
```

---

## Facts

| | |
|---|---|
| Requirements | ~245 after the Part 7 trim (was ~520); ~120 in M1–M4 |
| Open decisions | D1–D5, D13, D17, D37, D41–D44 (Part 7 §134) |
| Top risks | **S1 scope collapse**, S2 building the graph too early, S3 injection reaching a write tool, S7 interest fading before M2 |
| ADRs | ADR-001…013 |
| Hardware target | 16–32 GB RAM, modern CPU, optional GPU. No degradation tiers |
| Corpus target | **Your actual corpus** — measure it before assuming the spec's 100k files |

---

## Conventions

| | |
|---|---|
| `PREFIX-NNN` | Requirement ID. Permanent, never reused |
| `§N` | Section number, continuous across all seven parts |
| `Dn` / `Rn` / `Sn` / `ADR-n` | Decision / risk / solo risk / architecture decision |
| **[ASSUMPTION]** / **[COUNSEL]** | Part 4 only — both moot under Part 7 |

Numbering is append-only; superseded sections are annotated, not renumbered. Drift between Part 6 §106–119 and the eventual code is a defect in the docs.

**Warning to carry into the project README:** this indexes files you point it at. Don't point it at anything you wouldn't want an LLM to read.
