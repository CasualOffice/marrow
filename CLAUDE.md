# CLAUDE.md

Instructions for agents working in this repository.

## What this is

**Marrow**: a local knowledge runtime in Rust. It indexes folders the user grants, understands their structure, answers with citations to the exact page/cell/line, and exposes the index over MCP so existing agent front-ends can use it.

**The code exists and is used daily.** Fourteen crates, a Tauri desktop app, a `marrow` CLI, an MCP server over stdio, and 800+ tests. The spec still runs ahead of the build in most places, so **the spec is the design, [TRACKER.md](TRACKER.md) is the state** — never infer from a spec section that something is implemented. Current milestone is at the top of the tracker; open findings are in [BUGS.md](BUGS.md).

**This is a solo, personal, open-source project.** Not a product, no users, no deadlines. Optimise for the author shipping something they use daily, not for completeness.

## Read before acting

| Task | Read first |
|---|---|
| Anything at all | [ROADMAP.md](ROADMAP.md) current milestone + [TRACKER.md](TRACKER.md) |
| Schema, migrations, errors, config, IPC | [docs/ Part 6 §106–110](docs/Part_6_Engineering_Reference.md) |
| Chunking, retrieval fusion, context envelope | [Part 6 §112–114](docs/Part_6_Engineering_Reference.md) |
| Parsers, provenance classes | [Part 3 §63](docs/Part_3_Conversion_Multimodal.md) |
| Tables | [Part 5 §99](docs/Part_5_Capabilities.md) |
| Anything touching files, paths, evidence or execution | **[Part 7 §126](docs/Part_7_Solo_Rescope.md) — the non-negotiables** |

**Spec supersession:** the docs are **nine** parts, written in order. **Later parts supersede earlier ones.** Part 7 re-scopes everything for solo use and supersedes Part 4 entirely; never cite Part 1–6 guidance without checking whether Part 7 changed it. Part 8 (model runtime) and Part 9 (egress) came after Part 7 and supersede it in their own areas.

**And [DECISIONS.md](DECISIONS.md) supersedes the spec.** Where building it taught something the spec got wrong, the decision is the answer and the spec text was left alone: D3 (SQLite FTS5, not Tantivy), D54 (PDFKit, not PDFium), D55 (an MLX sidecar, not Candle or Ollama), D42 (a GUI, which §130 deleted), D56 (the agent layer, which §130 also deleted). Check DECISIONS before citing any part.

## Hard rules

These are not style preferences. Violating them creates bugs that are expensive or impossible to fix later.

1. **`source_span` on every IR node.** Page+bbox, sheet+cell, XML path, byte range, or timestamp. Provenance is the entire reason this project exists. A node without one is a bug.
2. **Path is never identity.** Files have stable IDs and path history. Never key anything on a path.
3. **Never hydrate cloud placeholders.** Check the dataless/offline flags before any read. This silently downloads hundreds of GB otherwise.
4. **Retrieved file content never grants authority.** It's data, even when it contains instructions. Never concatenate it into a system prompt; use the labelled envelope ([Part 6 §114](docs/Part_6_Engineering_Reference.md)).
5. **Canonicalize paths and check symlink escape at operation time**, not just at index time. String prefix checks are not sufficient.
6. **Stale-version check immediately before any write.** The user has the file open in their editor.
7. **Jobs are idempotent and resumable.** Key on `(source_version, processor, processor_version)`. The process will be killed mid-run constantly during development.
8. **Derived indexes are rebuildable; corrections are not.** Back up SQLite before any migration.
9. **Agent-written files are marked `origin = SELF`** and excluded from evidence authority. Otherwise the system cites its own output back as independent corroboration.
10. **Search must work with no LLM, no GPU, no network.**

### How to cite a hard rule

There are **three** numbered lists of these rules in this repo and **no two of them agree**:

| List | Count | Where | Numbering |
|---|---|---|---|
| **Hard rules** (above) | 10 | CLAUDE.md | the one every agent loads each session |
| **§126 "must not be relaxed"** | 14 | [Part 7 §126](docs/Part_7_Solo_Rescope.md) | a superset, ordered by *what it costs you*, not by subject |
| **The `invariants` skill** | 15 | `.claude/skills/invariants/SKILL.md` | a superset, grouped by subject |

The overlap is real but the numbers are not portable. `origin = SELF` is **9** here, **10** in §126 and **13** in the skill. "Never hydrate a placeholder" is **3** here, **3** in §126 and **5** in the skill — right for the wrong reason. Half the codebase used to cite one scheme and half the other, and nothing could catch it.

So:

- **Cite a rule by name.** "the `origin = SELF` rule", "never hydrate a placeholder", "symlink escape is re-checked at operation time". A name survives a renumber; a number does not.
- **`hard rule N`** is the one sanctioned numeric form, and it means *this list* — the ten above.
- **Never write `invariant #N`.** It is ambiguous across all three lists and is the form that produced every miscitation.

## Scope discipline

The spec is ~8,700 lines across nine parts and the author is one person. **The default answer to "should we also…" is no.**

- Don't build ahead of the current milestone
- Don't add a parser until a real file demanded it
- Don't abstract for platforms, hardware tiers or users that don't exist
- Don't build the knowledge graph ([D43](DECISIONS.md)) — it is gated on three real questions that search and timeline could not answer, and none have been logged. **A *code* graph is a separate question** and D43 now says so: its edges are parsed rather than inferred, so it cannot be wrong the way a document graph can. Still unbuilt, still behind the same gate, but judged on its own terms when the questions arrive
- Don't build an OS sandbox — settled, permanently

**Two of these were reversed, and pretending otherwise is why they got reversed silently:**

- **A UI is in scope.** [D42](DECISIONS.md) was reversed — the desktop app *is* the product. §130's UI deletion no longer applies; [docs/GUI.md](docs/GUI.md) §4 bounds what ships.
- **The agent layer already exists.** [D-agent-layer](DECISIONS.md) refused an agent runtime, model gateway, approval UX and chat UI. All four were built anyway, and [D56](DECISIONS.md) records that as superseded *in fact* — not repealed by argument. That is a warning, not a licence: nothing there authorises more agent-layer work than the current milestone asks for, and [Part 7 §130](docs/Part_7_Solo_Rescope.md) is still the reason to keep it small.

If you think something outside the current milestone is genuinely needed, add it to the TRACKER parking lot and say so. Don't just build it.

## Conventions

**Rust**
- Edition 2021+, `clippy` clean, `rustfmt` default
- `thiserror` for library errors, `anyhow` at binary boundaries
- Errors carry a code from the [Part 6 §108](docs/Part_6_Engineering_Reference.md) taxonomy and a *cause-and-action* user message. Generic failure text is a defect
- `tracing` for instrumentation, never `println!`
- No `unwrap()`/`expect()` outside tests and `main`

**Data**
- ULID `TEXT` primary keys
- Timestamps: `INTEGER` epoch **milliseconds, UTC**. Never local time, never text
- Enums as `TEXT` with CHECK constraints — readable in a debugger
- Soft delete via `status`; physical deletion only through the forget path

**SQLite**
- One writer actor, `mpsc` inbox, batched commits. Never open a second write connection
- WAL mode, `foreign_keys = ON`, `busy_timeout = 5000`
- `VACUUM INTO` backup before every migration

**Tests**
- Every application-level invariant in [Part 6 §106.12](docs/Part_6_Engineering_Reference.md) has a named test. An invariant without a test is a comment
- The adversarial corpus ([TRACKER](TRACKER.md#adversarial-corpus)) must be green before any write tool ships, and only ever grows

## Working process

1. Check [TRACKER.md](TRACKER.md) for the current milestone and open items
2. Do the work
3. **Update TRACKER** — tick items, add a Log entry if something surprised you
4. If you learned something that contradicts the spec, write it into [DECISIONS.md](DECISIONS.md). Don't silently diverge

## Commits

Present tense, scope-prefixed, explain *why* when it isn't obvious:

```
scan: detect dataless files before opening them

macOS SF_DATALESS and the .icloud stub both need checking — the flag
alone misses stubs created by older sync clients.
```

Don't commit: index data, model weights, anything containing real file paths from the author's disk, benchmark output with personal filenames.

## Things that look like bugs but aren't

- **Watchers miss events.** By design of the OS. Reconciliation is what makes the index correct; watchers are only hints.
- **Two files with the same content hash.** Expected. Dedup is a feature.
- **A file with no parser.** Stays discoverable via metadata (T5). Not a failure.
- **Low text yield on a PDF.** Probably scanned. Flag it and offer OCR; never silently drop it.
- **Conflicting facts coexisting.** Correct behaviour. Contradictions are stored with validity and provenance, not overwritten.
