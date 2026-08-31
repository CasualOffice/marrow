# Marrow — Research landscape

## What eight chat, RAG and agent products do, and what that means for a product surface Marrow now has

**Status:** Assessment, not specification. Nothing here is a requirement.
**Date checked:** 2026-08-31. Feature sets move weekly; every claim below is
from each project's own repository and documentation on that date.
**Companion:** [Comparison.md](Comparison.md) already covers this field on the
*safety* axes — provenance, injection, write-tool primitives, egress, self-citation.
This document covers the axis it does not: **what these things are like to use**,
and what Marrow's desktop window is missing when set beside them.

---

# 1. What was examined, and why

The author asked for it, and the reason it is worth asking now is
[D42](../DECISIONS.md): the desktop app *is* the product surface. Until that
reversal, Marrow's front-end was Claude Code and the only sensible comparison
was against other MCP servers. With a window that holds a conversation, streams
an answer and renders citations, the honest comparison set is the products
people actually keep open all day.

Eight were examined:

| Project | Shape |
|---|---|
| **opencode** (`sst/opencode`) | Terminal coding agent, 75+ providers, conversations in SQLite |
| **Open WebUI** | Self-hosted chat front-end with a large RAG stack behind it |
| **LibreChat** | Multi-provider chat with heavy conversation-management features |
| **AnythingLLM** | Workspace-oriented document Q&A, desktop app and server |
| **Jan** (`menloresearch/jan`) | Local-first chat over local models — Tauri, Rust backend, Node front-end |
| **LobeChat** | Chat plus an organisational layer: projects, pages, plugins, memory |
| **gemini-cli** | Terminal agent with filesystem, shell and web tools |
| **aider** | Terminal pair-programmer, git-native |

Two of them matter more than the rest and for opposite reasons. **Jan** is the
closest architectural sibling Marrow has — the same stack, the same premise —
so what it does differently is informative rather than merely different.
**opencode** publishes a security posture that happens to settle an argument
Marrow already settled, which is the most useful single finding in this pass.

---

# 2. The field

| Project | Local models | Retrieval over your own files | Citation granularity | Permission / execution posture | The one thing worth taking |
|---|---|---|---|---|---|
| **opencode** | Via provider, 75+ | Repo-scoped: Read / Glob / Grep / List / LSP | None — it quotes what it read | **Tiered tools + agent modes.** `build` (default), `plan` (denies every edit tool globally), `explore` (read and search only). Pattern-matched permission rules with wildcards, doom-loop detection, interactive approval. **Explicitly not a sandbox** | The passive tier, and the honesty about what it is for |
| **Open WebUI** | Yes | Yes — 9 vector DBs; Tika / Docling / Mistral OCR / PaddleOCR-vl extraction; hybrid BM25 + vector with reranking, plus a full-context mode | Document / chunk | Plugin surface: Filters, Actions, Pipes, Tools, Skills; MCP / MCPO / OpenAPI tool servers | **`#` to pull one document into the conversation**, and **queueing a message while the model is still answering** |
| **LibreChat** | Via provider, 30+ | Files per conversation | Document | **Sandboxed** code interpreter — Python, Node, Go, C/C++, Java, PHP, Rust, Fortran | **Search across every message and conversation**; forking a message or a thread; artifacts (React / HTML / Mermaid) with fullscreen preview and Mermaid export to SVG/PNG |
| **AnythingLLM** | Yes — bundled | Yes — workspaces, drag-and-drop ingestion, LanceDB / PGVector / Pinecone / Chroma, 40+ providers with dynamic routing | **Source citations** — document-level | No-code agent builder with "intelligent skill selection"; scheduled cron tasks | Drag-and-drop ingestion as the primary way files arrive |
| **Jan** | Yes — HuggingFace models, OpenAI-compatible server on `localhost:1337` | No document index | — | MCP support | **Published memory guidance in plain numbers**: 3B needs 8 GB, 7B needs 16 GB, 13B needs 32 GB |
| **LobeChat** | Via provider | No first-class file corpus | — | 10,000+ tools and MCP plugins, function calling, agent builder, scheduled runs | **Projects and Pages** as an organising layer above threads; structured, editable "personal memory" |
| **gemini-cli** | No | Filesystem, shell, web fetch and search tools | None | **Trusted Folders** — execution policy per folder. Sandboxing documented | Per-folder trust, and `GEMINI.md` as per-project context |
| **aider** | Via provider | Maps the whole repository to work in large codebases | None | **Git is the safety model.** Auto-commits every change with a generated message; undo is `git`, not pre-approval | Undo cheap enough that pre-approval stops being the only defence |

---

# 3. What this confirms about decisions already made

## 3.1 opencode says out loud what §129 decided quietly

opencode's security documentation states:

> opencode does not sandbox the agent. The permission system exists as a UX
> feature to help users stay aware of what actions the agent is taking — it is
> not designed to provide security isolation.

[Part 7 §129](Part_7_Solo_Rescope.md) reached the same conclusion by a
different route — that a sandbox protects unknown users from a malicious model,
and Marrow has no unknown users — and
[D-sandbox](../DECISIONS.md) records it as settled, permanently. The value of
opencode's sentence is not that it agrees. It is that **the most-starred
open-source coding agent, with a full permission system built and shipped,
declines to call that system a security control.** Marrow's structured argv and
env allowlists (§129.2, EXEC-007/009) are kept on exactly the same footing: bug
prevention and quoting hygiene, not isolation. Anyone who reopens the sandbox
question can now be pointed at a project that built the whole approval surface
and still says it is UX.

The corollary is the part worth watching. If permissions are UX everywhere,
then the thing that actually stops a poisoned README reaching a write tool is
the boundary below the model — the envelope ([§114](Part_6_Engineering_Reference.md)),
the write primitive's re-canonicalisation at operation time, and the adversarial
corpus. Those are hard rules 4, 5 and the M5 gate, and none of them is a prompt.

## 3.2 Jan validates the stack, and the stack is the boring part

Jan is Tauri with a Rust backend in `src-tauri` and a Node front-end in
`web-app`, running local models from HuggingFace behind an OpenAI-compatible
local server. That is Marrow's stack, arrived at independently:
[D42](../DECISIONS.md) chose Tauri 2 + React over one Rust core, and
[D55](../DECISIONS.md) chose a local sidecar over a cloud dependency, having
first checked that the Rust-native alternatives did not exist in publishable
form. A 44k-star project on the same shape is not proof the shape is right, but
it does remove "nobody ships desktop apps this way" as a concern.

Jan's memory guidance — 3B/8 GB, 7B/16 GB, 13B/32 GB — is coarser than what
Marrow's Models page already computes (S1: weights, KV, runtime buffers,
resident embedder and OS reserve, with the arithmetic shown). Marrow is ahead
here and should stay ahead, because [D59](../DECISIONS.md) established that KV
is ~160 KB per token and therefore an architectural property rather than a
function of parameter count — which is precisely what a rule of thumb keyed on
parameter count cannot express.

## 3.3 gemini-cli's Trusted Folders is per-root policy under another name

gemini-cli attaches execution policy to a folder rather than to a global
setting. [D47](../DECISIONS.md) made `.gitignore` respect **per-root policy**
rather than a global default, for unrelated reasons (442 of 475 spreadsheets
lived in gitignored data directories). The convergence is worth noting because
the TRACKER parking lot already holds "UI to set a workspace's data
classification" — a per-workspace policy whose enforcement is live (LLM-032)
and which nothing but hand-editing can set. Trusted Folders is what that
control looks like when someone ships it.

---

# 4. What Marrow is missing

Five gaps, all on the desktop surface, all with a working implementation to
look at. None is a new idea; each is a thing every mature chat product has and
this window does not.

| # | Gap | Seen in | Where it lands here |
|---|---|---|---|
| 1 | **Scoping a question to one project inline** | Open WebUI's `#` command loads a named document from the library into the conversation | **This is [B3](../BUGS.md).** "whats STT?" returned MFA settings, TaskRanking and a Code of Conduct because the workspace is all of `~/Desktop/melp`. B3 has been open as a diagnosis without a shape; `#` is the shape |
| 2 | **Search across conversations** | LibreChat searches every message and every conversation | Downstream of [B7](../BUGS.md) — there is one session and no history, so there is nothing yet to search. B6/B7 put a thread list in the sidebar; search over it is the next step, not a separate feature |
| 3 | **Forking a message or a thread** | LibreChat — fork, edit, resubmit, continue, with branching | Nothing today. This is the cheap version of context control: rather than a "clear context" button, the user keeps the good half of a thread and diverges |
| 4 | **Queueing a message while the model is still answering** | Open WebUI | Nothing today. Marrow streams token by token over a Tauri channel (S5) and the composer is inert until it finishes. Cheap, and it is the difference between a window that feels responsive and one that blocks |
| 5 | **Exporting a rendered artifact** | LibreChat exports Mermaid to SVG and PNG; artifacts open fullscreen | Marrow draws `mermaid` fences and runs `html` fences in a sandboxed frame (S5, S7), and there is no way to get either out of the window except by screenshot |

Two honest notes on this list. Gap 1 is the only one that fixes a bug rather
than adding a feature, so it is the only one with an obvious claim on the
current milestone. And gaps 2–5 are all *chat-product* work, which is exactly
the category [§130](Part_7_Solo_Rescope.md) deleted and
[D56](../DECISIONS.md) recorded as having been rebuilt anyway. Filing them is
not the same as scheduling them; see §6.

Two things seen elsewhere that are **not** filed as gaps, with the reason:

- **aider's git-as-undo.** Auto-committing every change and relying on `git
  revert` is a genuinely cheaper undo than transaction snapshots. It is not
  adopted because Marrow indexes folders, not repositories — most roots on this
  machine are not under version control, so the mechanism would be absent
  exactly where it is needed. Snapshots and undo stay ([§126](Part_7_Solo_Rescope.md) #5).
- **AnythingLLM's drag-and-drop ingestion.** Already shipped: the desktop app
  gained folder-adding and file drop, which was the fix for "no way to add a
  folder from the app at all" (TRACKER Log, 2026-08-30).

---

# 5. What Marrow does that none of them do

Stated narrowly, because the surrounding claims are easy to overstate and
[Comparison.md §13](Comparison.md) exists to catch exactly that.

**1. The citation resolves to a location inside the file, not to the file.**
Every product here that cites at all cites a document or a chunk: AnythingLLM
shows source citations, Open WebUI shows retrieved documents, LibreChat shows
attached files. Marrow's `source_span` is a page and a bounding box, a sheet
and a cell, an XML path, a byte range or a timestamp — and hard rule 1 makes a
node without one a bug rather than a degraded result. That granularity is the
differentiator, and it is a narrow one:

> **What is not unique:** hybrid retrieval. Open WebUI does BM25 + vector with
> reranking and a full-context mode; several others fuse lexical and semantic
> branches. Marrow's RRF fusion ([§113](Part_6_Engineering_Reference.md)) is
> ordinary in kind. What differs is what a fused hit can be resolved *to*.

**2. Provenance classes travel with the answer.** [Part 3 §63](Part_3_Conversion_Multimodal.md)
grades every parse — T1/T2 exact, T3 degraded, T4 approximate — and the class
reaches the citation badge. PDFKit text is `Degraded` because it is what the
extractor read rather than what is on the page; OCR is `Approximate` because
every character was inferred from pixels ([D60](../DECISIONS.md)). No product
in this set distinguishes "the file says this" from "our converter thinks the
file says this", which means a converted EPUB and a native Markdown file are
cited with identical confidence.

**3. `origin = SELF`.** Agent-written files are marked at write time, the mark
survives a reindex, and such files are returned `citable: false` — so the
system cannot cite its own output back as independent corroboration (hard rule
9). Comparison.md §11 already records this as unimplemented across the
competitive set; nothing in this pass changed that. AnythingLLM, Open WebUI and
LobeChat all let an agent write into a workspace that is itself indexed, and
none of them track where the text came from.

**4. Cloud placeholders are never hydrated.** Hard rule 3, TIER-005, and the
only reason it is a differentiator is that everyone else's ingestion path is a
file reader that opens what it is given. On a machine with OneDrive or iCloud
that is hundreds of gigabytes downloaded by a background indexer nobody
watched. Marrow checks `SF_DATALESS` and `.icloud` stubs before any read and
indexes such files as metadata only.

**5. Search works with no model, no GPU and no network.** Hard rule 10, and
structurally true rather than aspirational: `marrow index` builds FTS5 and
`marrow embed` is a separate command you turn on. Of the eight, only
AnythingLLM's desktop build is comparably self-contained, and its retrieval is
vector-only — so pulling the model out leaves it with nothing.

---

# 6. What not to copy

The four features below are each individually reasonable, and all four are the
same mistake for this project.

| Do not build | Seen in | Why not |
|---|---|---|
| **A plugin marketplace with thousands of tools** | LobeChat: 10,000+ tools and MCP plugins | Marrow's answer to "more tools" is already MCP, and the tools that matter are the ones over its own index. A catalogue is a distribution problem, and there is no audience |
| **A no-code agent builder** | AnythingLLM's builder with "intelligent skill selection" | This is the agent runtime [D-agent-layer](../DECISIONS.md) refused, wearing a GUI. The refusal was already violated once; violating it again with a *builder* multiplies the surface rather than adding to it |
| **Scheduled / cron runs** | AnythingLLM, LobeChat | Nothing here needs to run while nobody is asking. [D44](../DECISIONS.md) keeps this a single binary with no daemon split precisely so background execution is not a thing that exists to schedule |
| **A built-in calendar** | Open WebUI ships one, alongside real-time channels | A calendar is not a knowledge runtime. This is the clearest example in the set of a project accreting adjacent product surface |

[Part 7 §130](Part_7_Solo_Rescope.md) deleted the chat UI, the approval UX, the
agent runtime and the model gateway from the critical path, on the grounds that
building a worse copy of tools already on the machine is how a solo project
spends a year. Three of those four now exist, and
[D56](../DECISIONS.md) records that as **superseded in fact, not repealed by
argument** — "a warning, not a licence". This section is what that warning
looks like when applied. The five gaps in §4 are filed because they make an
existing surface work; the four items here are refused because they would make
a new one.

The scope test that follows from the whole exercise: **a feature earns a place
if it makes a citation easier to reach or easier to trust.** Inline document
scoping does (it stops an answer drawing on four unrelated services).
Conversation search does, weakly (it makes yesterday's cited answer findable).
A calendar does not, and neither does a tool catalogue.
