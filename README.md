# Marrow — Local Knowledge & Agent Runtime

A local knowledge runtime. It indexes folders you point it at, understands their structure, answers questions with citations to the **exact page, cell or line**, and exposes all of it over MCP so the agent you already use — Claude Code, Codex, Cursor — can search everything you own.

**Status:** M0–M2 done. **M3 in progress** — the desktop app runs, PDFs carry per-character provenance, tables are not started. The model runtime ([Part 8](docs/Part_8_Model_Runtime.md) S1–S5, S7) is in, so the app answers questions from a local model with citations. M4 semantic is **partial**: vectors are built, and read by the desktop's Ask and `marrow search` — not yet by the desktop's Search view or the MCP `search` tool. [TRACKER.md](TRACKER.md) is the real state.
**Scope:** Personal project. One user, one Mac, open source. Not a product.
**Licence:** Apache-2.0

---

## Why this exists

`ripgrep` finds strings. An LLM with a folder mounted reads a handful of files and guesses. Neither one can tell you *where* an answer came from, notice when the source changed, or remember what you corrected last month.

The bet: the durable asset is a **provenance-backed knowledge representation** — files, structure, history, entities, evidence, corrections — not the model of the month. The model is replaceable. The knowledge is not.

Two invariants carry the whole design:

- **Deterministic before probabilistic.** Get facts from the file format, AST, Git and filesystem before asking a model to infer them.
- **Policy below the model.** Text inside a PDF is data, even when it contains instructions. It can never grant authority.

And one that applied because this is for personal use — until the build outgrew it:

- **~~Don't rebuild the agent layer.~~** The plan was: Claude Code already does that well, so build the index and expose it over MCP. Then [D42](DECISIONS.md) made the desktop app the product, and an app that answers questions needs a model runtime — so a supervisor, a local generator and a conversational Ask surface all exist now. Recorded as [D56](DECISIONS.md) rather than quietly dropped. The index is still the durable asset; the agent layer is not.

---

## What it does

```
$ marrow workspace add ~/Projects                     # grant it a folder
$ marrow index                                        # scan and record what changed
$ marrow search "auth refresh token"                  # lexical + filename + semantic, cited
$ marrow search --literal '});'                       # exact scan, ignores the index
$ marrow embed                                        # build semantic search on top
$ marrow watch                                        # keep the index fresh
$ marrow mcp                                          # serve the index to Claude Code
```

Plus the desktop app, which is where Ask lives: a question answered by a local
model, from retrieved chunks, with clickable citations.

### Still to come

```
$ marrow file ~/Projects/q2.xlsx                      # everything known about one file
$ marrow table sum ~/Projects/q2.xlsx 'Q2!B4:B18'     # computed, not read by a model
```

## What it will not do

Index your whole OS without asking · take destructive actions on its own · record your screen · recognise faces or voices · sync across devices · ship a mobile app · replace Git or filesystem ACLs · treat embeddings as truth.

It also deliberately does **not** build an OS sandbox ([§129](docs/Part_7_Solo_Rescope.md)) — settled, permanently. §130's other refusal, its own chat UI and agent loop, did not hold: both exist ([D56](DECISIONS.md)).

---

## Status and next step

| | |
|---|---|
| Current milestone | **M3 — desktop shell · PDF · tables** (tables not started) |
| What works today | The desktop app: search, Ask with a local model and citations, status, folder granting · CLI `workspace add` · `index` · `search` (`--literal`) · `embed` · `status` · `watch` · `mcp` · the MCP server, over 35,119 real files |
| Not wired yet | Semantic search reaches the desktop's **Ask** and `marrow search`. The desktop's **Search** view and the MCP `search` tool are still lexical-only |
| Measured | 15.6 s to index and chunk the corpus; **0–3 ms** queries; embedding the corpus runs at 6.4 chunks/s (~2¼ h) |

See **[ROADMAP.md](ROADMAP.md)** for phases and **[TRACKER.md](TRACKER.md)** for the live task list.

---

## Documentation

| File | What it is |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | How it's built, in under 200 lines |
| [ROADMAP.md](ROADMAP.md) | Milestones M0–M7, scope rules, stop rules |
| [TRACKER.md](TRACKER.md) | Live checklist — the file that actually gets updated |
| [DECISIONS.md](DECISIONS.md) | Open and settled decisions, with reasons |
| [BUGS.md](BUGS.md) | Open findings — what is wrong right now, and what it misleads you into believing |
| [docs/](docs/README.md) | The full specification — nine parts, ~8,700 lines |
| [CLAUDE.md](CLAUDE.md) | Instructions for agents working in this repo |

**Reading the spec:** start with [docs/README.md](docs/README.md), then **Part 7** — it re-scopes the earlier commercial-product framing for solo use and supersedes Part 4. Later parts always supersede earlier ones, and **DECISIONS.md supersedes all of them** where building it proved a part wrong.

---

## Stack

What is actually built, and what each choice replaced. Anything marked *not
built* is still a plan.

```
Language     Rust · Tokio · serde · tracing
Filesystem   ignore · notify · blake3 · globset
Canonical    SQLite (WAL, single-writer actor)
Full text    SQLite FTS5 — same transaction as canonical state (D3, not Tantivy)
Vector       brute-force cosine over SQLite, indefinitely (D1); revisit past ~500k chunks
Parsers      Tree-sitter · PDFKit (D54, not PDFium) — in-process; the isolating
             subprocess is not built · calamine for tables (not built)
Embeddings   MLX in a worker process (D55, not Candle); Ollama/LM Studio detected if present
Generation   MLX worker under a supervisor with admission, a breaker and a KV cache (Part 8)
Interface    Desktop app · CLI · MCP server over stdio
UI           Tauri 2 + React + TypeScript — the desktop app is the product (D42, reversed)
```

Target hardware: an Apple Silicon Mac with 16 GB (D5). **No degradation tiers** — if it doesn't run here, that's a bug, not a configuration. PDF parsing and the model runtime are macOS-only by decision (D54, D55); elsewhere those files stay findable by name.

---

## The non-negotiables

Cheap to build in from day one, expensive or impossible to retrofit:

- **Path ≠ identity** — stable file IDs plus path history
- **`source_span` on every node** — provenance is the entire reason to build this
- Authority class and evidence on every derived fact
- Idempotent, resumable jobs; every index rebuildable from canonical state
- **Cloud placeholders are never silently hydrated** — that's your bandwidth and disk
- Untrusted-content boundary — file text never grants tool authority
- Path canonicalization and symlink escape checks
- Stale-version check before any write; snapshots and undo
- Backup before migration — derived data is disposable, your corrections are not
- Search works with no LLM, no GPU and no network
- Unicode NFC/NFD normalization — a correctness bug wearing a locale costume

Full list with reasoning: [Part 7 §126](docs/Part_7_Solo_Rescope.md).

---

## ⚠️ Before you point this at a folder

**It indexes what you tell it to. Don't point it at anything you wouldn't want an LLM to read.**

Once it's wired to a write-capable agent, a poisoned README in a cloned repo is aimed at your home directory. That's why the injection defences stay in even though this is a single-user project — see [Part 7 §126](docs/Part_7_Solo_Rescope.md).

---

## Contributing

Personal project, no SLA, no roadmap commitments. Issues are welcome as discussion; treat the tracker as mine. See [SECURITY.md](SECURITY.md).
