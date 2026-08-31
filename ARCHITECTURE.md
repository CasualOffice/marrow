# Architecture

Under 200 lines, by [Part 7 §133](docs/Part_7_Solo_Rescope.md). The design lives
in [docs/](docs/README.md); where building it contradicted the design, the answer
is in [DECISIONS.md](DECISIONS.md). **This file is the map, not the state** —
what is finished, half-finished and wrong is [TRACKER.md](TRACKER.md) and
[BUGS.md](BUGS.md).

## Shape

Fourteen crates, two binaries, one child process.

```
  Claude Code / Codex / Cursor            marrow-desktop  (Tauri 2 + React)
             │                                       │
             │ MCP over stdio                        │ tauri::invoke
             │ (`marrow mcp`)                        │
             ▼                                       ▼
  ┌────────────────────────────────────────────────────────────────┐
  │ adapters    mcp · cli · desktop      three frontends, one core │
  │ ────────────────────────────────────────────────────────────── │
  │ read path   query                    write path   ingest       │
  │             branches · fusion                     scan→parse→  │
  │             explain · file intel                  chunk→index  │
  │ ────────────────────────────────────────────────────────────── │
  │ substrate   core · store · scan · parse · index                │
  │ alongside   hw · model · tools · net                           │
  └───────────────────────────────┬────────────────────────────────┘
                                  │ spawn, when inference is asked for
                        ┌─────────▼──────────┐
                        │ python mlx_worker  │  an OOM kills the worker,
                        └────────────────────┘  never the index

  One SQLite file holds all three: canonical rows, the FTS5 index, the vectors.
```

**One database, not three stores.** [D3](DECISIONS.md) replaced Tantivy with
SQLite FTS5 for one reason: the index update commits in the *same transaction* as
the canonical row it derives from, so there is no window where the two disagree
and no dual-write to reconcile. The vectors joined the same file for the same
reason ([D1](DECISIONS.md): brute-force cosine, no ANN index). Separate boxes for
"lexical" and "vectors" would draw an architecture this project does not have.

**No daemon.** Two binaries, `marrow` and `marrow-desktop`, plus the Python MLX
sidecar either of them spawns when a question or an embedding needs a model
([D55](DECISIONS.md)). [Part 6 §110](docs/Part_6_Engineering_Reference.md)'s IPC
contract stays deferred until background indexing with nothing open earns the
split ([D44](DECISIONS.md)).

## The crates, and why the seams are there

| Crate | What it does |
|---|---|
| `core` | The vocabulary: IDs, error codes, `SourceSpan`, `TierState`, `Origin`. No I/O, no SQL. Three types encode invariants that are awkward to violate rather than merely documented |
| `store` | The canonical SQLite state, one writer actor with an `mpsc` inbox, unlimited `query_only` readers. Owns the migration runner and the backup-before-migrate rule |
| `scan` | What is on disk and what may safely be read. Produces values only — no writes, no cache. Owns containment, NFC path identity and the never-hydrate check |
| `parse` | Bytes in, IR out. Reads no files and opens no sockets, so every test in it is a string literal. A chain of parsers sorted by tier, terminating in metadata-only |
| `index` | Two things for opposite reasons: `fts5`, fast and only as fresh as the last write; `literal`, slow and exact and correct when the index is empty. Plus `vector`, brute-force cosine |
| `ingest` | The write path. Joins `scan` → `parse` → `store` → `index` and owns nothing itself |
| `query` | The read path, the mirror of ingest. Joins `store` and `index`. Owns no state, caches nothing, and depends on the `TextIndex` *trait* rather than on FTS5 |
| `hw` | What this machine can run (a probe, once) and whether it may run right now (a sampler, under 1 ms). Admission reads the sampler, never the probe |
| `model` | Which model, may it run now, who is waiting. Owns the decisions and no inference — the worker is a separate process precisely so an OOM kills it and not the index |
| `tools` | The three creation tools, through one guarded write path. Everything written is `Origin::SelfWritten` — not a parameter, there is no constructor that says otherwise |
| `net` | The one tool that can tell someone else something. Egress, ingress and SSRF handled in separate modules; it cannot write, cannot build a URL, cannot carry a cookie |
| `mcp` | The stdio protocol loop and eleven tools. An adapter: it computes nothing the CLI cannot also reach |
| `cli` | `marrow`. The composition root for everything except the window — including `marrow mcp`, which is a subcommand, not a second binary |
| `desktop` | `marrow-desktop`. Opens the store, registers commands, shows a window. The folder picker runs in Rust; the WebView has no filesystem affordance, only named commands |

The seam that matters most is between `parse` and everything else: a parser is
handed bytes and a `FileProbe`, never a path. It cannot read a placeholder
because it cannot read anything.

## Where the data lives

Everything is under `~/.local/share/marrow/`, per-user, never machine-wide, and
shared by both binaries so indexing from a terminal and searching from the window
see one database. `MARROW_DATA_DIR` overrides it.

```
~/.local/share/marrow/
├── marrow.sqlite          canonical + FTS5 + vectors, one file
├── models/                weights, content-addressed
├── dropped/               the workspace the app creates for dropped files
├── preferences.json       non-secret only; a provider key lives in the keychain
└── net-allow.txt          the hosts the user has agreed a fetch may reach
```

Inside the database, the distinction is whether losing a table costs anything
(`schema_meta` and `devices` are bookkeeping and belong to neither list):

- **Canonical** — `workspaces`, `workspace_roots`, `files`, `file_versions`,
  which carry `origin_device_id` ([D49](DECISIONS.md)); `self_written`, which is
  what stops the system citing its own output back as corroboration; and
  `conversations` / `conversation_turns`, the one thing the app holds that is not
  derived from the disk.
- **Derived** — `parse_results`, `ir_nodes`, `chunks`, `jobs`, `file_paths`,
  `table_ir`, `table_cells`, `text_index_docs`, `text_index`,
  `chunk_embeddings`. Delete them and a reindex rebuilds them.

The migration chain is numbered **across** crates — `store` owns 1, 3, 5, 6 and
`index` owns 2 and 4 — because `index` depends on `store` and store cannot
reference it back. A composition root passes `marrow_index::MIGRATIONS` and
nothing else, and `check.sh` fails if any file outside `crates/index/src/lib.rs`
names an individual migration. A hand-written subset shipped once and made every
CLI command fail against the app's own database ([D57](DECISIONS.md)).

## How a file becomes a citable chunk

```
walk ─▶ [1024] ─▶ probe + tier ─▶ [512] ─▶ hash ─▶ [256] ─▶ record the file
 1 thread            (inline)                N threads         then, per file,
                                                     parse ─▶ chunk ─▶ index doc
```

Bounded channels, so the slowest stage sets the pace and nothing buffers the
corpus. Probe and tier fold into the walk because `scan` already produces them
from the `lstat` the walker had to do anyway. The parse result, the tables, the
chunk rows and their FTS5 rows all reach the one writer in a single closure, so
they commit together or not at all — the D3 property, at the level of one file.

**One file never ends a run.** Recording a file is required; parsing it is
optional, and an optional stage that fails lands in the outcome's failure list
with its error code rather than being swallowed — "34,102 files, 11 could not be
parsed" and "34,102 files" call for different actions. A file nothing can parse
is not a failure at all: the chain terminates in a metadata-only artifact and
the file stays findable by name.

**Where a chunk's span actually is.** `text_index_docs.source_span` is `NOT NULL`
and is what a search hit cites. The canonical side is thinner than it looks:
`chunks.root_node_id` points at `ir_nodes`, and **nothing writes `ir_nodes`** —
ingest parses straight to chunks. So `rebuild_from` reads the span through a
`LEFT JOIN` that always returns `NULL` and falls back to `SourceSpan::Whole`. The
text survives a rebuild; the precise spans do not. That gap is on the TRACKER
parking lot, next to the citation work it blocks.

## How a question becomes an answer

Search is the load-bearing half and it runs with **no model, no GPU and no
network**. `query::search_hybrid` retrieves per branch to a fixed candidate
depth, fuses with RRF, applies the two post-fusion multipliers that are
correctness rather than taste — self-written content and degraded provenance —
and hydrates paths. `SearchResults::branches` names the branches that actually
ran, because a thin answer presented as a complete one is its own defect. The
semantic branch is strictly additive and its absence is never an error; not every
caller passes a vector index, and TRACKER says which ones have caught up.

Ask, in the desktop app, is retrieval plus a local 4B: search, take the top
chunks, bound the evidence in **bytes** before assembly, build the context
envelope, stream. The envelope ([Part 6 §114](docs/Part_6_Engineering_Reference.md))
is the mechanism behind "retrieved content never grants authority" — a
per-envelope unpredictable delimiter, regenerated on collision, with untrusted
content never last. It is defence in depth; the refusals hold whether or not the
model complies.

Every surface reports freshness or says it cannot ([D58](DECISIONS.md)). A stale
index answers confidently about a disk it has not looked at, and the counts are
*true* — they are just true about a disk that has moved on.

## Concurrency

- **One SQLite writer**, a single actor batching 500 rows or 100 ms. A lone
  write therefore waits up to 100 ms; callers who mind use `send` + `flush`
- **Jobs are idempotent and leased**, keyed on
  `(source_version, processor, processor_version)`. Kill it anywhere; it resumes
- **Parsers are not isolated yet.** `catch_unwind` in the router catches a Rust
  panic, records `PAR_WORKER_CRASH` and moves to the next tier — but not a
  segfault in a Tree-sitter grammar's C code. That subprocess is not built
- **Watchers produce hints, never truth.** Reconciliation is what makes the
  index correct, and both watchers sweep once before they start listening

## Deliberately not built

| Not building | Why |
|---|---|
| OS sandbox | A sandbox protects unknown users from a malicious model. You are the operator and run arbitrary shell all day. Settled permanently ([D-sandbox](DECISIONS.md)) |
| Knowledge graph | Gated on naming three questions you actually asked that search and timeline could not answer. None logged ([D43](DECISIONS.md)) |
| Daemon + IPC | Two binaries until background indexing with nothing open is worth the split ([D44](DECISIONS.md)) |
| ANN vector index | 60k chunks × 768 floats is exact and single-digit milliseconds. Reconsider past ~1M chunks; the code says so once rather than degrading quietly ([D1](DECISIONS.md)) |
| Non-macOS builds | PDFKit, Vision, FSEvents and `SF_DATALESS` are the platform. Elsewhere the placeholder check fails closed, so a build would index nothing ([D5](DECISIONS.md)) |
| CPU-only inference | This is MLX on Apple Silicon. There is no CPU path to be slow on, which excludes those users entirely ([D59](DECISIONS.md)) |
| Multi-device sync | ULIDs and a nullable `origin_device_id` keep the door open. Nothing more |

The agent layer was on this list and is not any more. A desktop app, a model
supervisor and a conversational Ask surface were all built after
[D-agent-layer](DECISIONS.md) refused them, and [D56](DECISIONS.md) records that
as superseded *in fact*, not repealed by argument — a warning about how the drift
happened, not a licence for more of it.

## Where to look when

| Task | Read |
|---|---|
| Schema, migrations, errors, config | [Part 6 §106–109](docs/Part_6_Engineering_Reference.md) |
| Chunking, fusion, context envelope | [Part 6 §112–114](docs/Part_6_Engineering_Reference.md) |
| Parser tiers, provenance classes, tables | [Part 3 §63](docs/Part_3_Conversion_Multimodal.md), [Part 5 §99](docs/Part_5_Capabilities.md) |
| The desktop app's bounds | [GUI.md](docs/GUI.md) §4 and §9 |
| The model runtime, and egress | [Part 8](docs/Part_8_Model_Runtime.md), [Part 9](docs/Part_9_Egress.md) |
| What must never be relaxed | [Part 7 §126](docs/Part_7_Solo_Rescope.md) |
| What is actually enforced on a commit | [`check.sh`](check.sh) |
