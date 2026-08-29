# Architecture

Short by design ([Part 7 §133](docs/LKAR_Addendum_Part_7.md)). The full treatment is in [docs/](docs/README.md); this is the map.

## Shape

```
   Claude Code / Codex / Cursor          ← the agent layer. Already exists. Not ours.
              │
              │ MCP (stdio)
              ▼
   ┌──────────────────────────┐
   │  lkar  (one binary)      │
   │                          │
   │  cli │ mcp-server        │  ← interfaces
   │  ─────────────────────   │
   │  query planner           │
   │  retrieval (lex+vec+kg)  │
   │  ─────────────────────   │
   │  knowledge substrate     │  ← the part only we can build
   │  scan · watch · reconcile│
   │  IR · chunk · index      │
   │  provenance · timeline   │
   └───────────┬──────────────┘
               │ spawn
        ┌──────▼──────┐
        │ lkar-parse  │  ← one subprocess. A malformed PDF must not kill the indexer.
        └─────────────┘

   SQLite (canonical)  ·  Tantivy (lexical)  ·  vectors (derived)
```

**One binary, one subprocess.** No daemon, no IPC layer, no service. The daemon split in [Part 1 §7](docs/Local_Knowledge_Agent_Runtime_Master_Specification.md) is deferred until background indexing while the CLI is closed becomes something worth having ([D44](DECISIONS.md)).

## Data flow

```
folder grant → scan → probe (MIME, size, tier) → hash (blake3)
                                                    │
                                                    ▼
                                        parse → IR nodes + tables
                                                    │
                        ┌───────────────────────────┼──────────────────┐
                        ▼                           ▼                  ▼
                    metadata                  lexical text          chunks
                    (SQLite)                  (Tantivy)                │
                                                                       ▼
                                                                  embeddings
                                                                   (vectors)
```

Watchers produce *hints*, never truth. Reconciliation is what makes the index correct — see [Part 1 §2.6](docs/Local_Knowledge_Agent_Runtime_Master_Specification.md).

## The four things that make it different

| | |
|---|---|
| **Provenance** | Every IR node carries a `source_span`: page + bbox, sheet + cell range, XML path, byte range, or timestamp. Answers cite it. Without this, use `ripgrep` |
| **Authority classes** | `USER_ASSERTION` > `DETERMINISTIC_FACT` > `EXTRACTED_FACT` > `INFERRED_FACT` > `HYPOTHESIS`. A fact never loses its class, so you can always tell knowledge from guessing |
| **Untrusted boundary** | Retrieved content is serialized into labelled blocks with runtime-generated delimiters ([Part 6 §114](docs/LKAR_Addendum_Part_6.md)). The prompt is defence in depth; enforcement is independent of whether the model complies |
| **Tables computed, not read** | Numeric questions evaluate over a Table IR and cite the cells. Models translate the question and phrase the answer; they never do the arithmetic |

## Key types

```rust
File        { file_id, current_path, fs_identity, tier_state, origin, status }
FileVersion { version_id, content_hash, mime, parser_id, observed_at, supersedes }
IrNode      { node_id, kind, source_span, trust, text_hash }
TableIr     { header_rows, column_types, column_units, provenance_class }
Chunk       { chunk_id, text, context_prefix, text_hash, provenance_class }
Evidence    { version_id, node_id, extractor, extraction_method, observed_at }
Fact        { subject, predicate, object, authority_class, evidence_id, valid_from/to }
Job         { job_type, idempotency_key, priority, lease_owner, not_before }
```

Full DDL: [Part 6 §106](docs/LKAR_Addendum_Part_6.md). Trim to the M1 subset first — see [ROADMAP](ROADMAP.md#schema-staging).

## Storage layout

```
~/.local/share/lkar/           (or platform equivalent — per-user, never machine-wide)
├── config/settings.json       non-secret only; secrets live in the OS keyring
├── db/knowledge.sqlite        canonical state
├── text-index/                Tantivy — derived, rebuildable
├── vectors/<generation>/      derived, rebuildable
├── extraction-cache/          content-addressed, > 64 KB chunk bodies
├── transactions/<txn_id>/     pre-image snapshots for undo
└── logs/
```

Everything under `db/` is canonical. Everything else can be deleted and rebuilt. **Corrections and workspace policy live in `db/` and are the only truly irreplaceable data** — back it up before any migration.

## Concurrency

- **One SQLite writer.** A single actor with an `mpsc` inbox; batched commits (500 rows or 100 ms). Unlimited WAL readers. [Part 2 §50](docs/LKAR_Addendum_Part_2.md)
- **Jobs are idempotent and leased.** Keyed by `(source_version, processor, processor_version)`. Kill the process at any point; it resumes rather than restarting
- **Parsers are isolated.** One subprocess, resource-limited, killed on timeout

## Deliberate omissions

| Not building | Why |
|---|---|
| OS sandbox | Protects unknown users from a malicious model. You're the operator. [§129](docs/LKAR_Addendum_Part_7.md) |
| Agent runtime, model gateway, approval UX, chat UI | Your agent front-end already does this better. [§130](docs/LKAR_Addendum_Part_7.md) |
| Daemon + IPC | Single binary until background indexing is worth the split |
| Desktop UI | Deferred past M6. Build it when the CLI annoys you |
| Multi-device sync | ULIDs and `origin_device_id` keep the door open; nothing more |
| Knowledge graph | Gated on naming three questions you actually asked that needed it ([D43](DECISIONS.md)) |

## Where to look when

| Task | Read |
|---|---|
| Schema, migrations, errors, config | [Part 6 §106–109](docs/LKAR_Addendum_Part_6.md) |
| Chunking, fusion, context envelope | [Part 6 §112–114](docs/LKAR_Addendum_Part_6.md) |
| Parser tiers and provenance classes | [Part 3 §63](docs/LKAR_Addendum_Part_3.md) |
| Tables | [Part 5 §99](docs/LKAR_Addendum_Part_5.md) |
| Cloud placeholders, watcher limits | [Part 2 §45.1–45.2](docs/LKAR_Addendum_Part_2.md) |
| Verification and reversibility | [Part 2 §46–47](docs/LKAR_Addendum_Part_2.md) |
| What must never be relaxed | [Part 7 §126](docs/LKAR_Addendum_Part_7.md) |
