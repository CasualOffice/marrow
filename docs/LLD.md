# Marrow — Low-Level Design

**Status:** Design. Governs implementation from M1 onward.
**Scope:** Module boundaries, trait seams, concurrency, error strategy, testing seams.
**Companion:** [UX.md](UX.md) designs the surfaces; this designs what sits behind them.

---

## 1. The one rule

> **Dependencies point inward. The domain knows nothing about I/O.**

```
  marrow-desktop ──┬── marrow-cli ──┬── marrow-mcp     three adapters,
     (Tauri 2)      │                │                   one core
                    ▼                ▼
                   ┌────────────────────────┐
                   │      marrow-query      │   orchestration
                   └────────────┬───────────┘
                                │
        ┌───────────────┬───────┴───────┬──────────────┐
        ▼               ▼               ▼              ▼
   marrow-scan    marrow-parse    marrow-store   marrow-index
    (filesystem)    (content)      (sqlite)      (lexical/vec)
        └───────────────┴───────┬───────┴──────────────┘
                                ▼
                          marrow-core            domain only
                     types · ids · errors        no I/O, ever
```

`marrow-core` has no `std::fs`, no `rusqlite`, no network. If a type in core needs to *do* something, it belongs one layer out. This is the seam that makes D1 (vector store) and D3 (Tantivy vs FTS5) deferrable instead of load-bearing.

**Enforcement, not convention:** `crates/core/Cargo.toml` declares only `blake3`, `serde`, `thiserror`, `ulid`. Adding `rusqlite` there should feel wrong because the dependency list makes it obvious.

---

## 2. Patterns that earn their place

Named because they solve a specific problem here — not because they are patterns.

### 2.1 Ports and adapters — for the two open decisions

D1 and D3 are both still open ([DECISIONS.md](../DECISIONS.md)). The port lets M1 ship without settling them.

```rust
/// Lexical retrieval. Implemented by Tantivy or SQLite FTS5 (D3).
pub trait TextIndex: Send + Sync {
    fn upsert(&self, docs: &[TextDoc]) -> Result<()>;
    fn delete(&self, ids: &[ChunkId]) -> Result<()>;
    fn search(&self, q: &TextQuery) -> Result<Vec<TextHit>>;
    /// Everything here is rebuildable from canonical state. Non-negotiable.
    fn rebuild_from(&self, src: &dyn ChunkSource) -> Result<()>;
}
```

**The port is narrow on purpose.** Five methods, no lifetimes, no leaked engine types. A port that exposes `tantivy::Searcher` is not a port.

Ports in the system: `TextIndex`, `VectorIndex` (M4), `ContentParser`, `Clock`. That is the complete list. **Everything else is a concrete type** — a port for something with exactly one implementation and no pending decision is ceremony.

### 2.2 Chain of responsibility — parser tier fallback

Part 3 §63's T1→T2→T3→T5 model *is* a chain: try native, fall back on `Unsupported` or `ParseFailed`, degrade provenance as you go.

```rust
pub trait ContentParser: Send + Sync {
    fn id(&self) -> &'static str;
    fn version(&self) -> &'static str;
    fn tier(&self) -> ParserTier;
    /// Cheap check on a probe. Must not read the file.
    fn handles(&self, probe: &FileProbe) -> bool;
    fn parse(&self, input: ParseInput) -> Result<ParsedArtifact>;
}
```

The router owns the chain; parsers know nothing about each other:

```rust
for p in self.parsers_for(&probe) {          // sorted by tier
    match p.parse(input) {
        Ok(a)                                   => return Ok(a),
        Err(e) if e.code() == ParUnsupported    => continue,   // next tier
        Err(e) if e.code().isolates_to_one_file() => {
            warn!(parser = p.id(), %e, "parse failed, degrading");
            continue;
        }
        Err(e) => return Err(e),                               // storage/policy: stop
    }
}
Ok(ParsedArtifact::metadata_only(&probe))     // T5 terminal, never a failure
```

**The chain always terminates in success.** A file with no parser stays discoverable via metadata (PAR-013). "No parser" is a fact about the file, not an error.

### 2.3 Actor — the single SQLite writer

WAL permits one writer. Scanner, parsers and indexers all produce writes. Rather than distributing lock discipline across every caller, one thread owns the write connection and everyone else sends messages.

The pattern is chosen because the constraint is **exclusive ownership of a resource**, which is exactly the problem actors solve. It is not chosen because actors are fashionable.

```
producers ──mpsc(bounded)──▶ writer thread ──▶ WAL
                                  │
                            batch: 500 rows or 100ms
```

Bounded channel gives backpressure for free: during the initial scan, producers block instead of buffering the corpus in RAM.

**Deliberately not** an actor framework. One thread, one `mpsc`, one loop.

### 2.4 Making illegal states unrepresentable — the invariants

Three invariants are enforced by types rather than by review:

| Invariant | Mechanism |
|---|---|
| #5 placeholders are never read | `TierState::safe_to_read()` gates every read path; the hasher returns `FsPlaceholderSkipped` for anything else |
| #13 self-written content can't be cited | `Origin::can_support_a_claim()`; the answer builder filters on it |
| #1 provenance is mandatory | `IrNode { span: SourceSpan, .. }` — not `Option<SourceSpan>`. There is no way to construct a node without one |

That last one is the highest-leverage line in the codebase. `Option<SourceSpan>` would make the invariant a habit; a non-optional field makes it a compile error.

### 2.5 Newtype — typed IDs

`FileId` and `VersionId` are both ULIDs. With bare `String`, swapping them is silent and produces a query that returns nothing. As newtypes it is a type error. Already implemented in `core::id`.

### 2.6 Staged pipeline with bounded channels — ingestion

Ingestion is a classic producer/consumer chain where stages have very different costs (walk: 97k files/s; hash: 4.2k files/s; parse: slower still).

```
walk ──▶ [chan 1024] ──▶ probe+tier ──▶ [chan 512] ──▶ hash ──▶ [chan 256] ──▶ parse ──▶ writer
 1 thread                  1 thread                   N threads              N threads
```

Bounded channels mean the slowest stage sets the pace and memory stays flat. No stage buffers the corpus.

**Not** a work-stealing framework, and **not** `rayon` across the whole pipeline: the stages have different parallelism requirements and one of them (the writer) must be strictly serial.

### 2.7 Read model — the file-intelligence panel

`marrow file` joins across a dozen tables. FI-005 forbids a separate store for it, so it's a **query-side projection assembled on demand**:

```rust
pub struct FileIntelligence { /* identity, metadata, structure, index_state, … */ }
impl FileIntelligence {
    pub fn assemble(store: &Store, id: FileId) -> Result<Self> { /* one read txn */ }
}
```

One read transaction, one snapshot, no cache to invalidate. At 9.4k files (M0) this is single-digit milliseconds; a cache would be pure liability.

---

## 3. Patterns deliberately rejected

Naming these matters as much as naming the ones we use — each was considered and would have cost more than it returned.

| Rejected | Why |
|---|---|
| **Generic `Repository<T>`** | Our queries are specific (`file_by_root_and_path`, `lease_next_job`). A generic CRUD trait would force every real query through an escape hatch, which is the abstraction failing while still charging rent |
| **Async everywhere / tokio in M1** | See §4. Nothing in M1–M2 multiplexes I/O. Async would add coloured functions, a runtime and `Pin` for zero benefit |
| **Event bus / pub-sub** | Two producers and one consumer. Direct channels are clearer and typed |
| **DI container** | Constructor arguments are dependency injection. Rust needs no framework |
| **Trait objects for closed sets** | `SourceSpan`, `TierState`, `ParserTier` are closed enums. `dyn` would trade exhaustiveness checking for nothing |
| **A `FileSystem` trait for testability** | Tempdirs are cheap and test the real syscalls, including the platform behaviour (NFD, `SF_DATALESS`) that a mock would paper over. **The bugs live in the real filesystem, so test against it** |
| **Builder for everything** | Only `WalkConfig` has enough optional fields to justify one |
| **Hexagonal purity in the frontends** | `cli` and `desktop` are adapters. They may call `store` and `scan` directly rather than routing every call through `query` |
| **A shared UI state machine mirrored in Rust** | The WebView owns its own view state (selection, pane sizes). Mirroring it into the core would create two sources of truth for something only the UI cares about |

---

## 4. Concurrency: threads, not async — for now

**Decision: the core stays synchronous. Tauri brings its own runtime; it does not get to colour our functions.**

Tauri 2 is async internally, so `tokio` enters the dependency tree at the desktop adapter. That does not mean the core becomes async. Tauri commands may be declared `async` and immediately hand work to a blocking pool:

```rust
#[tauri::command]
async fn search(state: State<'_, Core>, q: String) -> Result<SearchResults, UiError> {
    let core = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || core.search(&q)).await?
}
```

The core is called from a blocking thread and never knows a runtime exists. `marrow-cli` and `marrow-mcp` link the same synchronous core with no runtime at all.

| Workload | Nature | Right tool |
|---|---|---|
| Walk | Syscall-bound, sequential | one thread |
| Hash | CPU-bound | thread pool |
| Parse | CPU-bound, isolated | subprocess pool |
| SQLite write | Strictly serial | one actor thread |
| Lexical/vector search | CPU-bound, short | caller's thread |
| MCP over stdio | **One** connection, request/response | blocking loop |

Nothing here multiplexes thousands of sockets, which is the problem async solves. Threads plus bounded channels are simpler to reason about, simpler to debug, and produce readable stack traces.

**The rule that matters:** async lives at the adapter edge, never in `core`, `store`, `scan`, `parse`, `index` or `query`. Async is contagious — the moment a core function is `async`, every caller becomes async and the CLI inherits a runtime it has no use for. Keeping the boundary at the adapter is what lets three frontends share one core.

**Revisit the core's synchrony when** — and only when — the model gateway needs concurrent HTTP calls (M4+). Even then, prefer a blocking client on a thread pool.

### Cancellation

A shared `CancelToken` (an `Arc<AtomicBool>` plus a `Condvar` for sleepers) is checked at every stage boundary and every loop iteration over a batch. `Ctrl-C` sets it. Requirement: honoured within 500 ms ([UX §10](UX.md)).

Every long-running loop takes `&CancelToken`. Not a global; passed explicitly, so a function's signature tells you whether it can be interrupted.

---

## 5. Error strategy

One error type for the whole workspace: `marrow_core::Error`, already built. Consequences:

| Rule | Reason |
|---|---|
| **No per-crate error enum** | Six error types with six `From` chains is bookkeeping. One type with a `Code` discriminant gives the same matching precision |
| `Code` is a **closed enum in core** | Adding a code is a deliberate act in one file, so the taxonomy can't sprawl |
| `anyhow` only in `main()` | Libraries return `Result<T, Error>`; the binary is allowed to be lazy at the very top |
| Every user-facing error carries a **cause and an action** | SUP-001. Enforced by a test asserting message length and by review |
| `code.isolates_to_one_file()` drives the loop | A parse failure logs and continues; a storage failure aborts. The *type* decides, not the call site |
| No `unwrap`/`expect` outside tests and `main` | `#![deny(clippy::unwrap_used)]` at crate roots |

### The panic boundary

Parsers run in a subprocess. A panic or segfault there kills the child, the parent records `ParWorkerCrash` against that file, and indexing continues (NFR-001). **This is the only place a crash is tolerated**, and it's why the subprocess exists at all.

---

## 6. Key seams

The four places the design must stay flexible, and nowhere else.

| Seam | Trait | Open decision | Implementations at M1 |
|---|---|---|---|
| Lexical index | `TextIndex` | **D3** Tantivy vs FTS5 | one, behind the port |
| Vector index | `VectorIndex` | **D1** settled: brute force | M4 |
| Content parsing | `ContentParser` | — | text, md, code, toml/json, csv |
| Time | `Clock` | — | `SystemClock`, `FixedClock` in tests |

`Clock` exists solely so lease-expiry and staleness logic is testable without sleeping. It is one method:

```rust
pub trait Clock: Send + Sync { fn now(&self) -> Timestamp; }
```

Everything else is concrete. **Four seams is the budget.** A fifth needs a reason in `DECISIONS.md`.

---

## 7. Module layout

```
crates/
├── core/          types, ids, errors                    ← no I/O
├── store/         sqlite: schema, migrate, writer, read
├── scan/          walk, tier, probe, hash, path safety
├── parse/         parser trait + router + T1 parsers    (M1)
├── index/         TextIndex port + adapter              (M1)
├── query/         planner, fusion, result assembly      (M1)
├── mcp/           stdio server                          (M2)
├── cli/           argument parsing + rendering
└── desktop/       Tauri commands + events               (M3)
    └── ui/        React + TypeScript
```

`cli` and `desktop` contain **no logic**, only argument/command parsing and rendering. Anything it computes is something MCP can't get — see [UX §1](UX.md). The render layer is a pure function of a result type:

```rust
fn render(results: &SearchResults, out: &mut impl Write, style: Style) -> io::Result<()>
```

Pure function of data → testable with golden files, and `--json` becomes a second renderer over the identical input rather than a parallel code path.

The desktop adapter is the same shape: a command handler deserializes, calls `query`, serializes. **If a Tauri command contains an `if` that isn't error handling, the logic is in the wrong crate.** The test for this: every command handler should be readable in under ten lines.

---

## 8. Hot path

`marrow search "auth refresh"` — the sequence that must stay under 50 ms to first result.

```
cli::parse_args
   └─ query::Planner::classify(q)          → intent + branch set
        ├─ branch: lexical   ──▶ TextIndex::search       ~5 ms
        ├─ branch: path      ──▶ Store::path_match       ~1 ms
        └─ branch: semantic  ──▶ VectorIndex::search     ~120 ms  (M4)
             │
             ▼
        query::fuse(RRF)  ──▶ hydrate from Store  ──▶ render
```

**Lexical and path results render before the semantic branch returns.** The renderer takes a stream, not a `Vec`; the fusion stage emits a first pass and a re-ranked second pass. This is the single most important structural decision in the query layer, and it is why `render` accepts an iterator.

---

## 9. Testing seams

| Level | What | Where |
|---|---|---|
| Unit | Pure logic: path canonicalization, fusion math, chunk boundaries, type inference | in-module `#[cfg(test)]` |
| Property | Round-trips: IR→chunk→IR, hash hex, span serde, ULID ordering | in-module |
| Integration | Real SQLite, real tempdirs, real symlinks | `crates/*/tests/` |
| Golden | Rendered output, `--json` schema | `crates/cli/tests/golden/` |
| Invariant | The named tests from Part 6 §116.3 | asserted individually by `check.sh` |
| Adversarial | Injection, path escape, zip-slip, self-poisoning | `tests/adversarial/` (from M5) |

**Do not mock the filesystem.** M0 proved the interesting behaviour is platform-specific — NFD normalization, `SF_DATALESS`, `filter_entry` semantics. A mock would have passed while the real thing was broken.

**Golden files for rendering.** UX regressions are invisible to unit tests and obvious in a diff.

---

## 10. Extension points

The only places new capability plugs in without touching existing code:

| To add | Do this | Don't |
|---|---|---|
| A file format | Implement `ContentParser`, register in the router | Add a branch to a `match` on extension |
| A GUI view | Add a route + a component over an existing `query` call | Add a Tauri command that computes something |
| A search branch | Add a `Branch` variant + a weight in config | Hard-code a weight |
| A CLI command | Add a subcommand + a renderer | Put logic in the command |
| An MCP tool | Wrap an existing `query` function | Re-implement the query |
| A migration | Add a numbered file; the runner picks it up | Mutate the base schema |

**If adding a format requires touching more than the parser crate, the router is wrong.**

---

## 11. What "production grade" means structurally

Not features. These properties:

- [ ] `core` has zero I/O dependencies — checkable by reading one `Cargo.toml`
- [ ] Every port has ≥1 test that swaps the implementation
- [ ] No `unwrap`/`expect` outside tests and `main` — lint-enforced
- [ ] Every derived index rebuildable from canonical state, with a test that deletes and rebuilds it
- [ ] Kill the process at any point during ingestion → restart resumes, no duplicates, no partial rows
- [ ] Every long loop takes a `CancelToken` and honours it within 500 ms
- [ ] Parser crash kills one file, never the process
- [ ] One error type, closed code enum, every message names an action
- [ ] Rendering is a pure function — golden-tested
- [ ] No `async fn` in `core`, `store`, `scan`, `parse`, `index` or `query`
- [ ] Every Tauri command handler is under ten lines
- [ ] TypeScript command types generated from Rust, drift-checked in CI
- [ ] Four seams, no more, each tied to an open decision or a real second implementation
