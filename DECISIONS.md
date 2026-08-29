# Decisions

Only decisions that are still live under the solo scope. The full historical log (D1–D44) is in [Part 7 §134](docs/Part_7_Solo_Rescope.md) and [Part 2 §59](docs/Part_2_Gap_Closure.md); everything commercial (D19–D30) is void.

**Convention:** a decision moves to *Settled* only when it's been acted on. Record the reason, not just the choice — future-you will want to know why.

---

## Open

### D48 — Does MCP (M2) still come before the desktop shell (M3)?

**Needed by:** end of M1.

Now that the desktop app is the product, the milestone order is a real tradeoff:

| Order | For | Against |
|---|---|---|
| **MCP first, then GUI** *(recommended)* | Cheapest possible end-to-end test of the query API — 1–2 weeks. A bad API found through MCP costs a day; found after three panes are built on it, it costs a rewrite. If the GUI slips, the index is still useful daily via Claude Code | Delays the thing you actually want to look at by ~2 weeks |
| **GUI first** | Motivation. Risk **S7** says interest fades before something is usable daily, and a window is more motivating than a stdio server | Builds UI on an unvalidated query API |

**Recommendation: MCP first**, on the engineering argument. But S7 is a real risk and this is a call about your own motivation, which I can't make for you.

- [ ] Decide at end of M1

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

### D3 — Lexical index: Tantivy or SQLite FTS5? — **REOPENED by M0**

Was settled as Tantivy. M0 changes the input: the corpus is **9,435 files**, not 100k.

At that scale both are instant. FTS5 removes a dependency and a second index to keep consistent with canonical state; Tantivy's advantages (per-field BM25, pluggable tokenizers, richer analyzers) are real but barely exercised by 9.4k documents.

**Leaning FTS5.** Decide during M1 — the cost of being wrong is one rewrite of a small module, and Part 2 §36.3 already names "compact profile: SQLite FTS5 only" as a supported shape.

- [ ] Decide at M1

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
