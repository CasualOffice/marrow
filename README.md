# Marrow — Local Knowledge & Agent Runtime

A local knowledge runtime. It indexes folders you point it at, understands their structure, answers questions with citations to the **exact page, cell or line**, and exposes all of it over MCP so the agent you already use — Claude Code, Codex, Cursor — can search everything you own.

**Status:** `v0.0.1`. M0–M2 done; **M3 nearly done** (tables now read from CSV, Markdown, HTML, XLSX and DOCX; PDF *ruled* tables and `table compute` are not built). The model runtime ([Part 8](docs/Part_8_Model_Runtime.md)) answers questions locally, and cloud providers landed so a machine without a GPU still works. M4 semantic reaches the desktop's Ask **and its Search view** and `marrow search --semantic`; the MCP `search` tool is still lexical-only.

**Nothing before `v0.0.5` could answer a question on a machine that was not the one that built it.** Two separate faults, and fixing the first read like fixing both. `v0.0.0` left `mlx_worker.py` out of the bundle and declared no macOS folder-usage strings, so it could neither load a model nor read a granted folder — fixed in `v0.0.1`. But the *interpreter* the worker runs on was never in any bundle either: the app looked for `~/.local/share/marrow/runtime/mlx/bin/python`, which only ever existed because it had been created by hand on the author's Mac, and the printed fix began `python3.11 -m venv` — a command macOS does not ship. Every release verified "the worker script is in `Contents/Resources`", which is a check the build machine cannot fail. `v0.0.5` installs the runtime itself, from a digest-pinned archive, with no Python needed on the machine. [TRACKER.md](TRACKER.md) is the real state; [BUGS.md](BUGS.md) is what is currently wrong.
**Platform:** macOS on Apple Silicon. Windows and Linux **do not work yet** — see [Platforms](#platforms), which says exactly why and what a port needs.
**Scope:** Personal project, built in the open. One author. Not a product, no SLA.
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
$ marrow search "auth refresh token"                  # lexical + filename, cited
$ marrow search --semantic "how do I stop a run"      # also match on meaning
$ marrow search --type rs --since 7d "admission"      # filter by type, path, date
$ marrow search --explain "admission control"         # why each result ranked there
$ marrow search --literal '});'                       # exact scan, ignores the index
$ marrow embed                                        # build semantic search on top
$ marrow watch                                        # keep the index fresh
$ marrow mcp                                          # serve the index to Claude Code
```

Plus the desktop app, which is where Ask lives: a question answered from
retrieved chunks with clickable citations, conversations that survive a quit,
and a file you can drag onto the window and ask about immediately.

And a `read_table` MCP tool that hands an agent a grid — rows, typed values, and
the cell each one came from — rather than a wall of delimiters.

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
| Current milestone | **M3 — desktop shell · PDF · tables.** Tables read from CSV, Markdown, HTML, XLSX and DOCX with a header confidence and a span per cell; PDF ruled tables, unit extraction and `table compute` are not built |
| What works today | Desktop app — conversations that survive a quit, Ask with citations, drop a file in, first-run setup, Models, Status · CLI — `workspace add` · `index` · `search` (`--literal`, `--semantic`, `--explain`, filters) · `embed` · `status` · `watch` · `mcp` · MCP server, thirteen tools, over 35,404 real files |
| Not wired yet | Semantic reaches the desktop's **Ask** and `marrow search --semantic`; the desktop's **Search** view and MCP `search` are still lexical-only |
| Measured | ~13 s to index and chunk 35,404 files · **0–3 ms** lexical queries · embedding runs at 6.4 chunks/s · a 4B model answers in 3–13 s on an M-series laptop |

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
Parsers      Tree-sitter · PDFKit (D54, not PDFium) · Vision for image text (D60)
             · calamine for XLSX · zipped-XML reader for DOCX · tag scanner for
             HTML — all in-process; the isolating subprocess is not built
Embeddings   MLX in a worker process (D55, not Candle); Ollama/LM Studio detected if present
Generation   MLX worker under a supervisor with admission, a breaker and a KV
             cache (Part 8) · any OpenAI-compatible endpoint behind the same
             trait, key in the OS keychain (§140)
Interface    Desktop app · CLI · MCP server over stdio
UI           Tauri 2 + React + TypeScript — the desktop app is the product (D42, reversed)
```

---

## Platforms

Reference hardware is an Apple Silicon Mac with 16 GB ([D5](DECISIONS.md)). **No degradation tiers** — if it does not run there, that is a bug, not a configuration.

**Windows and Linux do not work yet, and the reason is specific rather than general.** Most of the code is already portable: the platform-specific pieces are `cfg`-gated and the workspace's Rust compiles for both targets. What stops it is one function.

`tier_from_metadata` decides whether a file is really on this disk or is a cloud placeholder — an iCloud stub, a OneDrive file marked recall-on-open. Hard rule 3 says a placeholder is **never** silently hydrated, because reading one downloads it, and that is your bandwidth and your disk. Only the macOS implementation exists. The stub for every other platform **fails closed**: it reports every file as unavailable, so nothing is read and nothing is hashed. A Linux or Windows build would run, refuse to index a single file, and be right to.

That is the correct default and it is not a port.

| | macOS (Apple Silicon) | Windows | Linux |
|---|---|---|---|
| Index, search, MCP, watch | ✅ | needs **TIER-002** — `FILE_ATTRIBUTE_RECALL_ON_OPEN`, `RECALL_ON_DATA_ACCESS`, `OFFLINE` | needs **TIER-004** — sync-client mount points, by config |
| Tables (CSV, MD, HTML, XLSX, DOCX) | ✅ | ✅ once indexing works | ✅ once indexing works |
| PDF text with per-character boxes | ✅ PDFKit ([D54](DECISIONS.md)) | ❌ no equivalent chosen | ❌ |
| Text in images (OCR) | ✅ Vision ([D60](DECISIONS.md)) | ❌ | ❌ |
| **Local** model | ✅ MLX ([D55](DECISIONS.md)) | ❌ Apple-only | ❌ Apple-only |
| **Cloud / OpenAI-compatible** model | ✅ | ✅ once the keychain has a backend | ✅ once the keychain has a backend |

**No GPU, or not a Mac? Use a provider.** Settings takes any OpenAI-compatible endpoint — OpenAI, OpenRouter, Together, Groq, or your own vLLM, LM Studio, llama.cpp or Ollama. Anthropic works through OpenRouter today; its native API is [parked](TRACKER.md). Search never needs a model at all, on any platform.

Honest cost of a port, in the order it would have to happen: cloud-placeholder detection per platform (the blocker above); a keyring backend other than `apple-native`; a hardware probe for those platforms; CI on real runners rather than cross-compiling — SQLite and Tree-sitter build C, so a Mac cannot honestly cross-check them. PDF and OCR would stay absent until someone picks replacements, and those files stay findable by name meanwhile.



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
