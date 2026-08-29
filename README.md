# Marrow — Local Knowledge & Agent Runtime

A local knowledge runtime. It indexes folders you point it at, understands their structure, answers questions with citations to the **exact page, cell or line**, and exposes all of it over MCP so the agent you already use — Claude Code, Codex, Cursor — can search everything you own.

**Status:** M1 complete — indexes and searches a real corpus. Currently at **M2**.
**Scope:** Personal project. One user, one machine, open source. Not a product.
**Licence:** Apache-2.0

---

## Why this exists

`ripgrep` finds strings. An LLM with a folder mounted reads a handful of files and guesses. Neither one can tell you *where* an answer came from, notice when the source changed, or remember what you corrected last month.

The bet: the durable asset is a **provenance-backed knowledge representation** — files, structure, history, entities, evidence, corrections — not the model of the month. The model is replaceable. The knowledge is not.

Two invariants carry the whole design:

- **Deterministic before probabilistic.** Get facts from the file format, AST, Git and filesystem before asking a model to infer them.
- **Policy below the model.** Text inside a PDF is data, even when it contains instructions. It can never grant authority.

And one that only applies because this is for personal use:

- **Don't rebuild the agent layer.** Claude Code already does that well. Build the index; expose it over MCP.

---

## What it will do

```
$ marrow search "auth refresh token"                  # lexical + semantic, cited
$ marrow ask "when does the Acme contract renew?"     # → contract.pdf p17, ¶Renewal
$ marrow file ~/Projects/q2.xlsx                      # everything known about one file
$ marrow table sum ~/Projects/q2.xlsx 'Q2!B4:B18'     # computed, not read by a model
$ marrow mcp                                          # serve the index to Claude Code
```

## What it will not do

Index your whole OS without asking · take destructive actions on its own · record your screen · recognise faces or voices · sync across devices · ship a mobile app · replace Git or filesystem ACLs · treat embeddings as truth.

It also deliberately does **not** build: an OS sandbox ([§129](docs/Part_7_Solo_Rescope.md)), or its own chat UI and agent loop ([§130](docs/Part_7_Solo_Rescope.md)).

---

## Status and next step

| | |
|---|---|
| Current milestone | **M2 — MCP server** |
| What works today | `workspace add` · `index` · `search` · `status`, over 35,119 real files |
| Measured | 15.6 s to index and chunk the corpus; **0–3 ms** queries |

See **[ROADMAP.md](ROADMAP.md)** for phases and **[TRACKER.md](TRACKER.md)** for the live task list.

---

## Documentation

| File | What it is |
|---|---|
| [ARCHITECTURE.md](ARCHITECTURE.md) | How it's built, in under 200 lines |
| [ROADMAP.md](ROADMAP.md) | Milestones M0–M7, scope rules, stop rules |
| [TRACKER.md](TRACKER.md) | Live checklist — the file that actually gets updated |
| [DECISIONS.md](DECISIONS.md) | Open and settled decisions, with reasons |
| [docs/](docs/README.md) | The full 7-part specification (~7,900 lines) |
| [CLAUDE.md](CLAUDE.md) | Instructions for agents working in this repo |

**Reading the spec:** start with [docs/README.md](docs/README.md), then **Part 7** — it re-scopes the earlier commercial-product framing for solo use and supersedes Part 4. Later parts always supersede earlier ones.

---

## Planned stack

```
Language     Rust · Tokio · serde · tracing
Filesystem   ignore · notify · blake3 · globset
Canonical    SQLite (WAL, single-writer actor)
Full text    Tantivy
Vector       brute-force cosine at M4 → LanceDB only when it hurts
Parsers      Tree-sitter · PDFium · calamine — in a subprocess
Embeddings   Candle, or an already-installed Ollama
Interface    CLI + MCP server over stdio
UI           Deferred past M6 — the CLI has to annoy me first
```

Target hardware: 16–32 GB RAM, modern CPU, optional GPU. **No degradation tiers** — if it doesn't run here, that's a bug, not a configuration.

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
