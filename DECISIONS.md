# Decisions

Only decisions that are still live under the solo scope. The full historical log (D1–D44) is in [Part 7 §134](docs/LKAR_Addendum_Part_7.md) and [Part 2 §59](docs/LKAR_Addendum_Part_2.md); everything commercial (D19–D30) is void.

**Convention:** a decision moves to *Settled* only when it's been acted on. Record the reason, not just the choice — future-you will want to know why.

---

## Open

### D45 — Product name

**Needed by:** before the first crate is named. Renaming later is cheap in code, annoying in docs.

Working codename is **LKAR**, which is an acronym, not a name. Candidates:

| Name | Why it fits | CLI feel | Notes |
|---|---|---|---|
| **Cairn** ⭐ | Stones *you* stack to mark a path so you can find your way back. Local, personal, durable, navigational — and a cairn is built incrementally, which is exactly what the index is | `cairn search "…"` | Recommended. Short, concrete, memorable, not tech-jargon |
| **Strata** | Layers of evidence laid down over time; geological provenance is literally the metaphor. Fits the timeline and version history | `strata ask "…"` | Strong second. Some existing use in identity/security products |
| **Quarry** | Double meaning: the place you extract stone from, and "to quarry" = to search diligently | `quarry find "…"` | Good verb-noun duality |
| **Marrow** | The dense substance deep inside; what you get to when you cut through | `marrow ask "…"` | Distinctive, low collision risk |
| **Tessera** | A single tile in a mosaic — one piece of evidence, meaningless alone, the picture only in aggregate | `tessera …` | Elegant metaphor, slightly obscure, longer to type |
| **Lodestone** | The naturally magnetized rock used as the first compass — it orients you | `lode search "…"` | Nice meaning, a bit long |
| **Fathom** | To understand deeply; also a measure of depth | `fathom ask "…"` | Collides with an analytics product |
| **Loam** | Rich soil — the substrate things grow from. "Knowledge substrate" is the spec's own phrase | `loam …` | Soft, pleasant, less distinctive |

**Recommendation: Cairn.** The metaphor is exact (a marker you build yourself, from what's already there, to navigate ground you'll cross again), it's four letters to type, and it isn't trying to sound like an AI product.

**Not checked:** crates.io / GitHub / domain availability. Worth 10 minutes before committing.

- [ ] Decide
- [ ] Rename crates, CLI binary, storage dir
- [ ] Find-and-replace `LKAR` in docs (leave the spec parts' internal references — they're historical)

---

### D43 — Build the knowledge graph at all?

**Gate, not a date.** Build it only when you can name **three questions you actually asked** that needed entity relationships and couldn't be answered by search + timeline.

Reason: [Part 2 §55 R2](docs/LKAR_Addendum_Part_2.md) rates graph quality as the highest-probability, highest-impact risk in the spec. It's also the easiest subsystem to build badly, and a bad graph is worse than none — it produces confident wrong relationships you then have to un-believe.

Questions logged so far:
1. _—_
2. _—_
3. _—_

---

### D42 — Build a UI at all?

**Deferred past M6.** CLI + MCP + your existing agent front-end covers it. Revisit when the CLI genuinely annoys you, and note *what* annoyed you — that's the UI spec.

---

### D1 — Vector store

**Deferred to M4.** Start with brute-force cosine; a few hundred thousand vectors is fine and has zero dependencies. Add LanceDB only when it measurably hurts. Decide with a benchmark on your own corpus, not a blog post.

### D2 / D31 — Embedding and LLM runtime

Two viable paths:
- **Candle** — one crate for embeddings and generation, fewer binaries
- **Call an installed Ollama** — zero size, zero maintenance, if you already run it

Decide at M4. Leaning Ollama-if-present, Candle as the fallback.

### D4 — PDF engine

**PDFium**, unless M0 shows you barely have PDFs. Pure-Rust options aren't there for page + bbox provenance, which is the whole point.

### D5 — Platform

**Your daily driver.** Don't abstract for OSes you don't run. Affects: watcher semantics, OCR engine (native on macOS/Windows, Tesseract on Linux), path canonicalization.

- [ ] Record which one: _—_

### D44 — Single binary or daemon split?

**Single binary + parser subprocess.** Revisit only if background indexing while the CLI is closed becomes worth it. That's when [Part 6 §110](docs/LKAR_Addendum_Part_6.md) (IPC contract) stops being deferred.

---

## Settled

### D41 — Project licence → **Apache-2.0**

Keeps every future door open, including relicensing your own code, and grants patent rights MIT doesn't. GPL dependencies remain usable when invoked as **separate processes** (ffmpeg via CLI, a Python sidecar) — only linking creates an obligation. See [Part 7 §128](docs/LKAR_Addendum_Part_7.md).

### D33 — Execution tier E2 → **merged into E4**

Not worth a separate tier for a single operator. See [Part 7 §129](docs/LKAR_Addendum_Part_7.md).

### D-sandbox — Build an OS sandbox? → **No, never**

A sandbox protects unknown users from a malicious model. You are the operator and run arbitrary shell all day. The reference implementation is reportedly ~17k lines. Structured argv and env allowlists stay — as bug prevention, not security controls.

### D-agent-layer — Build our own agent runtime, model gateway, approval UX, chat UI? → **No**

Claude Code / Codex / Cursor already do this well and are on the machine. LKAR is the index; MCP is the interface. This deletes roughly 60% of the spec from the critical path. See [Part 7 §130](docs/LKAR_Addendum_Part_7.md).

### D13 — OCR engine → **platform-native**

Free (0 MB) and better than bundled Tesseract on macOS and Windows. Tesseract only on Linux.

### D17 — GPS/location extraction → **off by default**

It's your own photo library; turn it on deliberately if you want it.

### D3 — Lexical index → **Tantivy**

BM25 and field-aware search without building it. SQLite FTS5 would be the fallback if Tantivy proves heavy, but there's no reason to expect that.

### D37 — Recipe format → **public, plain JSON**

You will hand-edit it. Don't invent a DSL.

---

## Void under solo scope

D10, D11 (business model, positioning), D19–D30 (commercial, compliance, distribution), D22 (managed inference), D25 (app stores). Retained in [Part 4](docs/LKAR_Addendum_Part_4.md) in case the project's scope changes.

---

## How to add one

```markdown
### Dnn — Short question

**Needed by:** milestone or trigger
**Options:** …
**Leaning:** … because …
**Decided:** date → choice, and the reason it won
```
