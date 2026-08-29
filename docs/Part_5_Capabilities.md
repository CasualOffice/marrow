# Marrow — Master Specification, Part 5

## Execution, Local Inference, Generative Media, File Intelligence, and Agent Capability Parity

**Status:** Addendum to Marrow Master Specification Parts 1–4
**Date:** 30 August 2026
**Numbering:** Continues from §92 of Part 4
**Format:** Tables and points only

---

# 93. Scope of this part

Parts 1–4 describe a system that **finds, understands and cautiously edits** files. They do not describe a system that **runs things, chooses its own local model, produces visual output, or reads a table numerically.** Those are the gaps this part closes.

| # | Capability requested | State in Parts 1–4 | Closed in |
|---|---|---|---|
| 1 | Script / command execution | §14.4 sketch; **§51 disables shell in V1**, defers to P6 | §97 — new `EXEC` block, tiered E0–E4 model |
| 2 | Local LLM support matched to the machine | §8.14 names adapters; no hardware probe, no model registry, no sizing logic | §95 `HW`, §96 `LLM` |
| 3 | Image generation | **Not mentioned anywhere** | §98 `GEN` |
| 4 | Metadata, tables and "everything we know about a file", arranged | `META` (§69) covers embedded metadata; tables are scattered across PAR-007, CHK-005/006 with no unified model | §99 `TBL` + `FI` |
| 5 | Agent capability parity with coding CLIs | §8.16, §15.1 sketch tools; no catalogue, no parity analysis | §100 `CAP` |
| 6 | "Whatever the user asks, we have an answer" | §12.1 lists 7 query modes | §101 — answer coverage matrix, 32 archetypes |

## 93.1 The constraint that governs all of it

Adding execution, local generation and richer extraction **expands the attack surface faster than it expands the feature set.** Every subsystem below is therefore specified against the Part 1 invariants, not alongside them:

| Invariant | Applied here |
|---|---|
| Policy below the model (§6) | Execution and generation are tools, gated identically to file writes |
| Untrusted content never grants authority (ADR-007) | A script suggested by file content is still untrusted; §97.6 |
| Deterministic before probabilistic (ADR-005) | §99 computes over tables rather than asking a model to read numbers; §98.1 prefers rendering over diffusion |
| Reversibility declared per invocation (§47) | Script execution is `Irreversible` unless proven otherwise |
| Graceful degradation (§1.2) | Every capability here is optional; the product works without all of them |
| **New — §98.6** | **Anything the system generates must never re-enter its own evidence graph as independent corroboration** |

---

# 94. Prior-art analysis

Surveyed for architecture decisions, not feature copying. Sources listed in §104.

## 94.1 Systems reviewed

| Project | Stack | What is directly relevant | What to take | What to avoid |
|---|---|---|---|---|
| **Spacedrive** | Rust + Tauri, "PRRTT" stack, single core crate, CQRS/DDD | Closest structural analogue: cross-platform Rust file explorer with a virtual index, BLAKE3 content identity, media-metadata module, durable task system, cross-platform fs-watcher | **CQRS action/query registry with auto-generated TypeScript types** — directly solves the §96 IPC contract problem in Part 6. Confirms BLAKE3 + CAS identity (§8.3) and a durable task engine (§20) | Its VDFS sync ambition. SYNC-001 already declares single-device; multi-device sync is what has historically consumed that project's roadmap |
| **Khoj** | Python, multi-client (desktop, Obsidian, Emacs, browser, mobile) | Personal-corpus RAG over PDFs, Markdown, notes, repos | The multi-surface distribution insight — an Obsidian/editor plugin is a cheaper stage-1 wedge than a full desktop app (§83.2) | Server-process-per-user model; no provenance-to-location discipline |
| **Onyx (ex-Danswer)** | Python, MIT core | 40+ connectors, agents, RAG, MCP, web search, code execution, deep research, file creation | Confirms the capability set users now expect by default — this is the §101 coverage bar | Server-side index and connector-centric design. It is C5 in §82.1, the category Marrow is explicitly not in |
| **Codex CLI** | Rust | OS-native sandboxing: Seatbelt (macOS), Landlock (Linux), RestrictedToken (Windows) — reportedly **~17k lines for the sandbox alone**, plus a MITM proxy for network filtering | The cost signal. This validates §51's honesty and sizes §97's E3/E4 tiers realistically | Nothing — this is the reference implementation for §97 |
| **Claude Code** | — | Deny-first permission model with allow/ask/deny rules; bubblewrap+seccomp on Linux with Landlock fallback; Seatbelt `.sbpl` on macOS; network reduced to loopback + proxy; layered defences that each can independently block | **Layered independent blockers** and **deny-first with human escalation** — exactly §6.1's trust model, validated in production. Read-before-edit guard and exact-string replacement (§100.3) | — |
| **OpenHands** | Python | Docker isolation + triple analyzer stack (static + LLM risk scoring) | The idea of a **risk classifier in front of execution**, complementing policy | Docker as the isolation primitive — §64.3 already rejects containers for consumer desktop |
| **mistral.rs** | Rust | Quantized models, Apple Silicon + CPU + CUDA, embeddable via the `mistralrs` crate | **Leading candidate for the in-process local LLM runtime** (§96.4) — embeddable in Rust with no daemon |
| **Candle** | Rust (HuggingFace) | LLaMA, Mistral, Phi, Gemma, StableLM; also ships a Stable Diffusion example (v1.5, v2.1, SDXL, Turbo) | One runtime for embeddings, LLM and diffusion — reduces binary count. Already a §36.2 candidate | Model coverage lags llama.cpp; verify each target model |
| **llama.cpp** (+ Rust bindings) | C++ | Broadest quantized model support, GGUF ecosystem | Fallback/compat layer (§96.4 R3) | FFI surface and build complexity across 3 platforms |
| **Ollama** | Go, HTTP | The de-facto local-model standard users already have installed | **Adapter, not dependency.** If the user already runs Ollama, use it — zero install cost, zero size cost (§96.4 R4) | Requiring it. A background HTTP daemon we do not control violates the §7 process model |
| **stable-diffusion.cpp** | C/C++ | Portable local diffusion, runs on modest hardware, has Rust bindings | The engine choice **if** G3 ships (§98.3) | Bundling it. §98.2 argues G3 is the lowest-value generative capability for this product |
| **Docling** (IBM) | Python | High-fidelity table + reading-order extraction | Its table-structure output model is a good IR reference | **Reported ~8 GB RAM per worker for the layout model.** Disqualifying for a consumer desktop under NFR-005. Do not adopt as the default path |
| **pdfplumber / Camelot / Tabula / Unstructured** | Python | PDF table extraction | pdfplumber reported as the most accurate open-source coordinate-based tool with default settings — best T3 sidecar table path (§99.7) | Camelot fails on text-drawn separators (common in generated financial PDFs). Tabula/`stream` and borderless tables need per-document tuning — not viable unattended |

## 94.2 Conclusions that change the spec

| # | Finding | Change |
|---|---|---|
| A1 | Spacedrive's CQRS registry with generated TS types is a better IPC design than a hand-maintained message catalogue | Adopt in Part 6 §110; every command is a registered action/query, types generated, no drift |
| A2 | Codex CLI's ~17k-line sandbox is the honest price of E4 shell execution | §97 tiers execution so that **useful** scripting (E1) ships without paying it |
| A3 | Onyx ships code execution, file creation, web search and deep research as table stakes | §101 coverage matrix must be answered explicitly, tier by tier, or Marrow reads as underpowered |
| A4 | Docling's memory profile shows high-fidelity table models are server-shaped | §99.7 routes tables through deterministic extraction first, ML layout second, and only on demand |
| A5 | Ollama is already installed on many target machines | §96.4 makes "use what is already there" the first branch of the model-selection tree |
| A6 | Candle can serve embeddings, LLM and diffusion | Resolves D2 in favour of Candle **if** benchmarks are close, on binary-size grounds alone |

---

# 95. Hardware capability probe — `HW`

Nothing in Parts 1–4 detects what the machine can do. Every local-inference decision below depends on it.

## 95.1 What is probed, once at first run and on hardware change

| Signal | Source | Used for |
|---|---|---|
| CPU cores (P/E split where available), ISA features (AVX2/AVX-512/NEON) | OS / CPUID | Worker counts (§21), quantization choice |
| Total + available system RAM | OS | Model sizing (§96.2), parser concurrency |
| GPU vendor, model, VRAM | Metal / DXGI / vulkan / nvml | Model sizing, acceleration EP selection |
| Unified memory (Apple Silicon) | sysctl | **Changes the sizing rule** — see §96.2 |
| NPU presence (ANE, DirectML-capable NPU) | OS | ONNX EP selection for embeddings |
| Accelerator EPs actually loadable | ONNX Runtime probe | Never trust a device list; try to load |
| Free disk on the index volume | OS | Index/model budgets, refuse-with-explanation |
| Power source, battery level, thermal state | OS | §21 scheduler, `EXEC`/`GEN` gating |
| Metered connection | OS | TIER-010 |
| Existing local runtimes (Ollama, llama.cpp server, LM Studio) | Port/socket probe + config | §96.4 R4 — use what exists |

## 95.2 Requirements

| ID | Requirement |
|---|---|
| HW-001 | Capability probe runs at first launch, on major OS update, and on detected hardware change. Result is cached with a probe schema version. |
| HW-002 | Probe is **non-destructive and fast** (< 2 s), and never blocks the UI. Accelerator load-tests run in a subprocess so a driver crash cannot kill the app (NFR-001). |
| HW-003 | Every accelerator claim is verified by actually initialising it once, not by reading a device list. |
| HW-004 | Probe results are visible to the user in Settings, in plain language: "This machine can run models up to ~8B at 4-bit." |
| HW-005 | The probe drives defaults only. The user may always override downward, and may override upward with an explicit warning. |
| HW-006 | Probe output contains no identifiers usable as a hardware fingerprint for entitlement (ENT-007). |
| HW-007 | Telemetry may report **bucketed** capability tiers only (TEL-002), never exact device strings. |
| HW-008 | A machine that fails the minimum bar for a capability gets an explanation and a working alternative, never a disabled button with no reason (SUP-001). |
| HW-009 | Thermal/battery state is re-read continuously and feeds §21, `EXEC`, `GEN` and Tier C (§48) gating. |
| HW-010 | Probe failure degrades to a conservative profile; it never blocks launch. |

## 95.3 Capability tiers

| Tier | Rough profile | Embeddings | Local LLM | OCR | Diffusion |
|---|---|---|---|---|---|
| **T-min** | 4 GB RAM, no GPU | ✗ (cloud or none) | ✗ | Platform-native only | ✗ |
| **T-low** | 8 GB RAM, integrated GPU | Small ONNX, CPU, slow | 1–3B @ Q4, slow | ✅ | ✗ |
| **T-mid** | 16 GB RAM, or 8 GB unified | ✅ CPU/NPU | 7–8B @ Q4 | ✅ | SD1.5-class, minutes |
| **T-high** | 32 GB RAM or 16 GB unified or 12 GB VRAM | ✅ accelerated | 13–14B @ Q4 | ✅ | SDXL-class |
| **T-max** | 48 GB+ unified, or 24 GB+ VRAM | ✅ | 30B+ @ Q4 | ✅ | FLUX-class |

**Free tier (§79.2) targets T-min.** Lexical search, metadata and `META` extraction must be fully usable at T-min with no model of any kind.

---

# 96. Local LLM support — `LLM`

## 96.1 Principle

> **The machine decides what runs; the user decides whether it runs; policy decides where it may run.** Marrow recommends, never silently downloads, and never blocks on a model.

## 96.2 Sizing rules

Derived from the standard memory arithmetic: roughly **2 GB per 1B parameters at FP16**, halved at Q8, quartered at Q4.

| Rule | Detail |
|---|---|
| Weight footprint | `params_B × 2 GB × quant_factor` (FP16 1.0, Q8 0.5, Q5 ~0.35, Q4 ~0.25) |
| KV cache | Add for the configured context length; it is not free and is the usual cause of "it loaded then died" |
| Headroom | Discrete GPU: keep ≥ 15% VRAM free. System RAM: target ~1.5× the model footprint, **16 GB floor** for a comfortable experience |
| Unified memory (Apple Silicon) | Budget against total unified memory minus an OS reserve; do not apply the discrete-VRAM rule |
| **Quantization preference** | **A larger model at Q4 generally beats a smaller model at Q8 for the same memory budget.** Default to Q4_K_M-class. |
| Fallback ladder | If the chosen model fails to load: drop quantization → drop parameter count → drop context length → offer cloud (policy permitting) → lexical-only. Never crash, never hang |
| Context sizing | Default context is chosen from measured memory, not from the model's maximum. §12.3 already budgets context; this bounds it physically |

## 96.3 Task-to-model routing

One model is the wrong answer. Marrow has four distinct inference jobs with different requirements:

| Job | Needs | Local candidate class | Cloud acceptable? |
|---|---|---|---|
| Embeddings | Throughput, small, multilingual (I18N-006) | Small ONNX/Candle encoder | Yes, if policy permits (EMB-006) |
| Reranking | Latency, small cross-encoder | Small ONNX cross-encoder | Rarely worth it |
| Answer generation | Instruction following, long context, citation discipline | 7–14B @ Q4 at T-mid/T-high | Yes — the main cloud use case |
| Tier C enrichment / structured extraction (§48) | **Reliable structured output** (MOD-009), throughput over eloquence | 3–8B @ Q4 with schema-constrained decoding | Yes, but §48 governor applies |

**Structured output is the hard constraint for the Tier C job**, not raw quality. A model that cannot be constrained to a JSON schema is unusable for graph extraction regardless of benchmark scores.

## 96.4 Runtime selection tree

| Order | Runtime | When | Cost |
|---|---|---|---|
| R1 | **Already-installed Ollama / LM Studio / llama.cpp server** | Detected by probe (HW-001) and user confirms | **0 MB, 0 maintenance.** Best possible outcome |
| R2 | **Embedded Rust runtime — mistral.rs or Candle** | Default for users with no existing runtime | In-process or subprocess; no daemon; ships with the app |
| R3 | llama.cpp via bindings | Fallback for GGUF models the embedded runtime lacks | FFI + 3-platform build complexity |
| R4 | OpenAI-compatible endpoint (private server, vLLM) | Enterprise (§22.2) | Config only |
| R5 | Cloud provider | Policy permitting, BYO key (§80.2) | Config only |

**Recommendation:** R1 first, R2 as the shipped default. R2 candidate selection (mistral.rs vs Candle) folds into decision **D2**, with a new tie-breaker — Candle can additionally serve embeddings and diffusion (§94.2 A6), reducing shipped binary count.

## 96.5 Requirements

| ID | Requirement |
|---|---|
| LLM-001 | A model registry records, per model: ID, family, parameters, quantization, footprint, context limit, licence (LIC-001/002), SHA, capabilities (tool calling, structured output, multilingual, vision), and minimum hardware tier. |
| LLM-002 | Model recommendations are computed from the HW probe (§95) and shown with the expected footprint and speed **before** download (PKG-013). |
| LLM-003 | Models are downloaded on demand, content-addressed and integrity-verified (PKG-011). Never bundled outside the air-gapped SKU. |
| LLM-004 | Model load failure is caught, reported in plain language, and falls back down the §96.2 ladder automatically. |
| LLM-005 | Structured-output capability is **tested, not assumed** — a probe prompt validates schema conformance before a model is offered for Tier C or tool use. |
| LLM-006 | Tool-calling capability is likewise probed. A model that fails is offered for answers only, not for actions. |
| LLM-007 | The active model, its quantization and its execution boundary are visible during every generation (UX-012, MOD-006). |
| LLM-008 | Local inference runs in a separate process (§7.1). An OOM kills the worker, never the daemon or the UI (NFR-001, §24). |
| LLM-009 | Inference is preemptible: an interactive query outranks Tier C enrichment (§21.2, §48.4). |
| LLM-010 | Local generation is throttled by thermal and battery state (HW-009). On battery, long generations require user confirmation. |
| LLM-011 | Air-gapped mode: models installed from local media, registry populated offline, zero network calls (SEC-015, PKG-006). |
| LLM-012 | Model change is a **generation change** for anything persisted — summaries, extracted facts, image descriptions (mirrors EMB-003, IMG-006). Prior outputs are not silently reinterpreted as coming from the new model. |
| LLM-013 | Licence and permitted use are shown per installed model (LIC-004); non-commercial models are user-installable but never bundled (LIC-003). |
| LLM-014 | The user may pin a model per workspace, overriding routing, subject to workspace data classification (MOD-003/004). |
| LLM-015 | A "why this model?" explanation is available, naming the hardware facts that drove the recommendation. |

---

# 97. Script and command execution — `EXEC`

## 97.1 Resolving the conflict with §51

Part 2 §51 states: *"Shell execution is disabled in V1. It requires V2 sandbox posture."* That remains correct **for arbitrary shell**. It is not correct as a blanket statement about scripting, and it has been costing the product a capability it can safely have.

The resolution is that "run a script" is four different requests with four different risk profiles:

| Tier | What it is | Process spawned? | Sandbox required | Phase |
|---|---|---|---|---|
| **E0** | No execution | — | — | Default, always available |
| **E1** | **Recipes** — deterministic pipelines over Marrow's own typed tools (§100.2). No interpreter, no OS process. | **No** | **None** — every step is already policy-gated | **P3** |
| **E2** | Allowlisted binaries, structured argv, no shell interpolation, workspace cwd, filtered env, resource limits | Yes | §51 V1 posture is sufficient | P3–P4 |
| **E3** | User-authored scripts in a vetted interpreter (Python/Node/POSIX sh) | Yes | **§51 V2 posture required** | P6 |
| **E4** | Arbitrary shell, model-generated command lines | Yes | V2 posture + explicit opt-in + per-invocation approval | P6, off by default |

## 97.2 Why E1 is the important tier

Most of what users mean by "can it run scripts?" does not need a process:

| User request | E1 recipe? | Steps |
|---|---|---|
| "Rename these 200 files to `YYYY-MM-DD_title`" | ✅ | search → extract date from `META` → plan renames → diff preview → transactional move |
| "Pull the totals from every invoice PDF into a CSV" | ✅ | search → table extract (§99) → normalize columns → write CSV |
| "Find every config with the old Redis URL and change it" | ✅ | Journey D (§39) — already specified |
| "Summarise what changed in this project last month" | ✅ | timeline query → summary |
| "Split this spreadsheet by region into separate files" | ✅ | read ranges → group → write |
| "Convert these 40 DOCX to PDF" | ⚠️ E2 | needs an external converter binary |
| "Run the test suite" | ⚠️ E2 | allowlisted `cargo`/`pytest`/`npm` with fixed argv |
| "Run this Python script I wrote" | E3 | interpreter |
| "`curl … \| sh`" | E4 | never by default |

**E1 gets the majority of the value at a fraction of the risk**, because every primitive it composes is already a typed tool with a declared risk class, reversibility class and validator (§47.2). A recipe is therefore *automatically* previewable, diffable, transactional and undoable. An arbitrary shell command is none of those.

## 97.3 Recipe model (E1)

| Property | Rule |
|---|---|
| Form | Declarative DAG of typed tool invocations. Not a Turing-complete language. |
| Control flow | `map` over a result set, `filter`, `if`, bounded `foreach`. **No unbounded loops, no recursion, no dynamic code.** |
| Data flow | Typed values only; outputs of one step bind to inputs of the next by schema |
| Authoring | Generated by the agent from a natural-language request, **or** hand-written by the user, **or** saved from a successful run |
| Preview | The whole DAG resolves to a concrete `PreparedAction` list before anything executes (§30.4), so §16.7's plan UI shows real targets |
| Reversibility | Computed as the weakest step (§47.3) |
| Persistence | Saved recipes live in the Agents surface (§16.1) with a version and an author |
| Re-run | A saved recipe re-runs against fresh search results; targets are re-resolved and re-approved, never replayed from cached paths |

## 97.4 Process execution rules (E2–E4)

Extends §14.4 and SEC-011 with the concrete posture, informed by Codex CLI and Claude Code (§94.1):

| Rule |
|---|
| **Structured invocation only:** `{executable, argv[], cwd, env_allowlist}`. Never a string passed to `sh -c`, `cmd /c` or PowerShell. |
| Executable resolved to a canonical path and checked against the allowlist **after** resolution, never before (symlink defence, SEC-002). |
| `cwd` restricted to a workspace root; path canonicalization per §6.3 applies to every argv element that resolves to a path. |
| Environment is an allowlist, not a denylist. Credentials, provider keys and `LD_PRELOAD`-class variables are never inherited. |
| Network denied by default. Where a runner legitimately needs it (package install, test fetch), it is a distinct, separately approved capability with a destination policy (AGT-011). |
| Resource limits mandatory: CPU time, wall time, memory (rlimit / job object), output bytes, and process count. Kill on breach. |
| stdout/stderr are captured, size-capped, and enter context as `UNTRUSTED_EVIDENCE` (AGT-005). |
| Exit code alone is not verification. Every runner declares a validator (§46.4). |
| **Deny-first with human escalation.** Unknown command → ask, never assume. Allow/ask/deny rules are per-workspace and user-editable. |
| Layered independent blockers: policy engine, allowlist, sandbox, resource limits, and approval each block independently. No single bypass is sufficient. |

## 97.5 Sandbox implementation

| Platform | E2 (P3) | E3/E4 (P6) |
|---|---|---|
| macOS | Separate process, restricted env, hardened runtime, resource limits | **Seatbelt** (`sandbox-exec` with a generated profile) or App Sandbox |
| Linux | Separate process + seccomp + Landlock where kernel ≥ 6.7 | **bubblewrap + seccomp**, Landlock mandatory; surface unavailability (WATCH-style honesty, §51) |
| Windows | Restricted token, low integrity level | **AppContainer / RestrictedToken** |
| All | rlimits, no network | + network namespace/proxy denial |

**Budget honestly:** the reference implementation in this space is reported at roughly 17k lines for the sandbox alone plus a filtering proxy. §51's caution was right. E1 exists so the product is not hostage to that schedule.

## 97.6 The injection rule for execution

This is the highest-severity interaction in the whole part.

| Rule |
|---|
| A command, script, recipe or argument that **originates in file content** carries no authority whatsoever (ADR-007). A `README` saying "run `make deploy`" is a quotation, not an instruction. |
| Model-proposed commands are always shown as **resolved, canonical, literal argv** at approval time — never as a model-authored summary (SEC-013). |
| Filenames are never interpolated into command strings. Command injection through a filename is an explicit adversarial test case (§29.4). |
| A recipe generated in a session where retrieved evidence contained instruction-shaped text is flagged for elevated scrutiny in the approval UI. |
| Files marked `provenance.external = true` (META-004, downloaded) elevate scrutiny further. |
| Execution capability is never widened by anything read, fetched or transcribed (§71.2). |

## 97.7 Requirements

| ID | Requirement | Tier |
|---|---|---|
| EXEC-001 | Execution is off by default at every tier above E1. | All |
| EXEC-002 | E1 recipes are a declarative DAG of typed tools with no interpreter and no process spawn. | E1 |
| EXEC-003 | Recipes have no unbounded loops, no recursion, no dynamic code generation. | E1 |
| EXEC-004 | A recipe fully resolves to concrete `PreparedAction`s before any step executes. | E1 |
| EXEC-005 | Recipe reversibility is the weakest step; irreversible steps are ordered last where the DAG permits (§47.3). | E1 |
| EXEC-006 | Saved recipes re-resolve targets on every run; cached paths are never replayed. | E1 |
| EXEC-007 | Process invocation is structured `{executable, argv[], cwd, env_allowlist}`. No shell string interpolation, ever. | E2+ |
| EXEC-008 | Executables are allowlisted per workspace, checked after canonical resolution. | E2+ |
| EXEC-009 | Environment is allowlist-based; secrets are never inherited. | E2+ |
| EXEC-010 | Network denied by default; a network-capable runner is a separate approved capability with destination policy. | E2+ |
| EXEC-011 | CPU, wall-clock, memory, output-size and process-count limits are mandatory and enforced by the OS, not by the app. | E2+ |
| EXEC-012 | Every runner declares a validator; exit code alone never constitutes verification (§46.4). | E2+ |
| EXEC-013 | stdout/stderr enter context as untrusted evidence, size-capped. | E2+ |
| EXEC-014 | Approval shows the literal resolved command, cwd and declared network access. | E2+ |
| EXEC-015 | E3/E4 are unavailable until the §51 V2 sandbox posture ships and is externally verified (§56 P6 gate). | E3/E4 |
| EXEC-016 | Sandbox unavailability (old kernel, missing bubblewrap) is surfaced and blocks E3/E4 rather than silently degrading. | E3/E4 |
| EXEC-017 | Execution runs appear in the action timeline with command, decision, exit state and validator result (AGT-015). | All |
| EXEC-018 | A global Stop (UX-010) terminates running processes and cancels queued steps. | All |
| EXEC-019 | Execution is denied on battery below a threshold and under thermal pressure unless the user overrides (HW-009). | E2+ |
| EXEC-020 | Adversarial corpus includes: command injection via filename, hostile `README` instruction, argv path traversal, env-var exfiltration, output that impersonates an approval, fork bomb, and network egress from a runner. | All |

---

# 98. Generative media — `GEN`

## 98.1 Three different things get called "image generation"

| # | Capability | Value to **this** product | Cost | Model needed |
|---|---|---|---|---|
| **G1** | **Deterministic rendering** — charts from spreadsheet ranges, diagrams from code structure or the entity graph, timelines from `ActivityEvent`, document/page previews, table screenshots for citation | **Highest.** It is the visual form of evidence the product already holds, and it is *citable* | Low | **None** |
| **G2** | **Image transformation** of assets already in the workspace — crop, resize, format convert, redact, extract embedded images, contact sheets | Medium. Real workflow value, deterministic, verifiable | Low | None |
| **G3** | **Text-to-image synthesis** (diffusion) | **Lowest.** It produces content the knowledge base did not contain, has no provenance, and cannot be cited | High (models, VRAM, hours, licensing) | Yes |

## 98.2 Recommendation

> **Build G1 first and treat it as an answer-rendering capability, not a generative one. G2 next. Ship G3 only as an optional, user-installed plugin, and never bundle it.**

Rationale: the differentiator is provenance (§82.2 #1). A rendered chart that links back to the exact spreadsheet range *is* provenance. A diffusion image is the only output this system can produce that has none — it is the one thing in the entire spec that cannot be cited.

| Phase | Ships |
|---|---|
| P2 | Sparkline/preview rendering in results (§16.3) |
| **P5** | **G1 full: chart rendering from tables (§99), graph neighbourhood rendering (§16.5), timeline rendering, page-region citation images** |
| P6 | G2: image transforms as typed tools with validators |
| Post-P7 | G3 as an optional plugin, if user demand justifies it |

## 98.3 If G3 ships — the constraints

| Item | Position |
|---|---|
| Engine | `stable-diffusion.cpp` (portable C/C++, Rust bindings, runs on modest hardware) or Candle's diffusion path if R2 already selected Candle (§96.4) |
| Hardware gate | T-mid minimum (§95.3). SD1.5-class at T-mid, SDXL-class at T-high, FLUX-class at T-max — FLUX needs roughly 12 GB+ VRAM |
| Download | Optional, on demand, size shown first (PKG-013). **+300 MB to several GB.** Never in the base installer |
| **Licensing** | **The blocking issue.** Model licences in this space vary sharply, and some widely used weights are non-commercial. LIC-001 permits redistribution only of commercial-use-permitted models; LIC-003 permits user-installed non-commercial models but forbids bundling. Every candidate needs individual counsel review (§88) |
| Compute | Minutes per image on CPU at T-mid. Subject to §48-class budgeting, idle+AC only (HW-009) |
| Safety | Content-safety posture, prohibited-use terms, and an explicit non-goal statement (no likeness generation of real people — consistent with IMG-015 / AUD-009) |

## 98.4 The self-poisoning rule — **applies to all generated and agent-written content**

This is a gap in Parts 1–4 that generation makes acute, and it is not limited to images.

> **Anything Marrow writes into a watched folder will be re-indexed by Marrow. Without marking, the system's own output becomes evidence that looks independent, and can corroborate the claim that produced it.**

| Rule |
|---|
| Every file created or modified by an agent action, recipe or generator is stamped with `provenance.origin = SELF`, the transaction ID, the model and the prompt/template version. |
| `SELF`-origin content is indexed and searchable — the user must be able to find it — but is **excluded from evidence authority** for answer verification (§46.3 step 2). |
| A claim cannot be supported by evidence whose sole origin is a prior Marrow output. Citation UX marks such sources distinctly. |
| Generated images are `SYNTHETIC`: they never produce `DETERMINISTIC_FACT`s from their (absent) EXIF, and captions of them are not evidence at all. |
| Where the platform supports it, apply durable content credentials (C2PA-class) to generated media, in addition to the internal flag. The internal flag is authoritative; the embedded credential is for the outside world. |
| The `SELF` flag survives copy and move within watched roots (content-hash carried), and its loss on export is expected and documented. |
| Adversarial test: a generated summary written to disk must not be usable to corroborate the claim it summarised. |

## 98.5 Requirements

| ID | Requirement |
|---|---|
| GEN-001 | G1 deterministic rendering is the default generative capability and requires no model. |
| GEN-002 | Every rendered chart, diagram or timeline carries provenance to the exact ranges, nodes or events it was rendered from, and is clickable back to them. |
| GEN-003 | A rendering that cannot establish provenance for its inputs is not produced. |
| GEN-004 | Rendered output states its source and generation time on the artifact itself when exported. |
| GEN-005 | G2 transforms are typed tools with declared reversibility and validators (§47.2); originals are never overwritten without a snapshot (§47.4). |
| GEN-006 | G3 is an optional, separately downloaded, non-bundled component; absence never degrades any other capability. |
| GEN-007 | G3 is hardware-gated by the §95 probe with a clear explanation when unavailable (HW-008). |
| GEN-008 | Model licence and permitted commercial use are shown before download and enforced against LIC-001/003. |
| GEN-009 | **All generated content is stamped `provenance.origin = SELF` and excluded from evidence authority (§98.4).** |
| GEN-010 | Generated media carry content credentials where the platform supports it. |
| GEN-011 | Generation is budget-governed exactly like Tier C (§48): demand-driven, idle+AC, preemptible, visible backlog. |
| GEN-012 | Generation prompts and outputs appear in the action timeline; cloud generation is disclosed in the egress UI (UX-013). |
| GEN-013 | Explicit non-goals: no likeness generation of real people, no deepfake tooling, no NSFW pipeline. Stated in the product, not only in policy. |
| GEN-014 | Generation is never permitted to write outside an approved workspace root, and file creation follows the normal R2 risk path (§14.1). |

---

# 99. File intelligence and table understanding — `FI` + `TBL`

The request: *"handling of meta and tables and info on files — whatever we could find and arrange."*

## 99.1 The File Intelligence panel (`FI`)

Everything the system knows about one file, in one place. All of it already exists in the schema; none of it is currently assembled.

| Section | Source | Confidence shown |
|---|---|---|
| Identity | `file_id`, content hash, size, MIME (probed, not extension — FS-014), duplicates by hash (FS-008) | Deterministic |
| Location | Current path, **path history** (FS-006), workspace, root, tiered-storage state (TIER-008) | Deterministic |
| "What this file told us about itself" | `META` (§69): EXIF, XMP, IPTC, OOXML core/app properties, PDF info, ID3, container tags, xattr/ADS download origin | Deterministic, confidence 1.0 |
| Structure | IR outline: headings, sheets, slides, symbols, tables, images, page count (§8.6) | Deterministic |
| **Tables** | Every table found, with dimensions and inferred column types (§99.2) | Per §99.7 tier |
| Text extraction health | Parser, version, yield, `LOW_YIELD` / `PARTIAL` flags, provenance class `EXACT`/`DEGRADED`/`APPROXIMATE` (CONV-003) | Explicit |
| Media derivatives | OCR pages + confidence, transcript segments, captions — each labelled by extraction method | Per §70.1 |
| Entities & relations | Entities mentioned, relations this file is evidence for, each with authority class | Per §11.2 |
| Timeline | Created/modified/renamed events, Git history, agent actions touching it | Deterministic |
| Links | Inbound/outbound: code imports, document links, spreadsheet references, email thread, archive parent, embedded-object parent |
| Provenance & trust | Download origin, `provenance.external`, `provenance.origin = SELF` (§98.4), signature where available |
| Index state | Parsed / chunked / embedded / graphed, generation IDs, pending jobs, last error |
| Actions | What the agent may do with it at current policy, with risk and reversibility per operation |

| ID | Requirement |
|---|---|
| FI-001 | The panel is available for every indexed file, including metadata-only (T5) files. |
| FI-002 | Every row states its extraction method and authority class; nothing is presented as flat "truth". |
| FI-003 | Unknown is shown as unknown. Absent metadata is never inferred to fill the panel. |
| FI-004 | Every item links to its evidence or its source location. |
| FI-005 | The panel is a **read model** assembled from canonical state, never a separate store. |
| FI-006 | It is fully keyboard-navigable and screen-reader complete (A11Y-001). |
| FI-007 | It exposes "why this file did/did not match" for the last query (§84.6 deflection). |
| FI-008 | It offers per-file actions: re-parse, OCR now (OCR-011), deep-understand (§48.2), exclude, forget. |

## 99.2 Tables as a first-class type (`TBL`)

Parts 1–3 treat tables as chunking edge cases (CHK-005/006, PAR-007). That is insufficient — a large share of the factual questions users ask are answered by a number in a table, and asking an LLM to read that number out of prose-flattened text is where hallucinated figures come from.

**Unified Table IR** — every source normalizes into one structure:

| Field | Meaning |
|---|---|
| `table_id`, `file_version_id`, `source_span` | Provenance: page + bbox (PDF), sheet + range (XLSX), XML path (DOCX), DOM path (HTML), byte range (CSV/MD) |
| `header_rows`, `header_cols` | Detected, with a confidence; not assumed to be row 0 |
| `cells[]` | `{row, col, rowspan, colspan, raw_text, typed_value, unit, formula, number_format, style_flags}` |
| `column_types[]` | Inferred: string, integer, decimal, currency, percent, date, datetime, duration, boolean, enum, id, formula |
| `units[]` | Detected from header text, number format or a units row (`$k`, `%`, `ms`, `kg`) |
| `merged_regions[]` | Preserved, never silently flattened |
| `provenance_class` | `EXACT` / `DEGRADED` / `APPROXIMATE` (CONV-003) |
| `relations` | XLSX formula dependencies, named ranges, cross-sheet references (PAR-007) |
| `caption`, `footnotes` | Anchored, because units and qualifiers usually live there |

## 99.3 Computing over tables instead of reading them

| Rule |
|---|
| A numeric question that maps to a table is answered by **evaluating over the Table IR**, then citing the cells — not by asking a model to read numbers from flattened text. |
| The model's job is to translate the question into an operation (`sum`, `filter`, `group`, `compare`, `lookup`, `delta`) and to phrase the result. The arithmetic is deterministic. |
| Every returned figure cites its exact cells. §46.3 step 4 (verbatim numeric match) then passes by construction. |
| If the operation cannot be expressed against the IR, the system says so rather than approximating. |
| Cross-table comparison requires an explicit key mapping, surfaced to the user; silent joins on similar headers are forbidden. |
| Unit mismatches block the operation and are reported (`$k` vs `$`, `%` vs ratio). This is a common source of confidently wrong answers. |

## 99.4 Chunking and retrieval for tables

| Rule | Ref |
|---|---|
| Table chunks carry headers and the caption on every chunk (CHK-005) | Existing |
| A workbook is never flattened into prose (CHK-006) | Existing |
| Large tables are chunked by row band with headers repeated, plus one **schema chunk** describing columns, types, units and ranges — which is what semantic search actually matches against | New |
| Column headers, sheet names and named ranges are boosted in lexical search like symbols (IDX-004 analogue) | New |
| A table hit returns the table region, not a stray row | New |

## 99.5 Extraction engine strategy

| Source | Engine | Provenance | Notes |
|---|---|---|---|
| XLSX / ODS | Native (calamine-class) + formula parser | **EXACT** — sheet + cell ref | Formulas, named ranges and number formats preserved (PAR-007) |
| CSV / TSV | Native | EXACT — byte range | Dialect + encoding + header detection |
| Markdown / HTML | Native | EXACT | Includes `colspan`/`rowspan` |
| DOCX / PPTX | Native OOXML | EXACT — XML path | Table structure is explicit in the format |
| **PDF, ruled tables** | Native, from PDFium line objects + text positions | EXACT — page + bbox | Deterministic; do this first |
| **PDF, borderless / text-aligned** | Coordinate clustering (pdfplumber-class, T3 sidecar) | DEGRADED | The hard case. Camelot-class lattice detection fails on text-drawn separators, common in generated financial PDFs |
| **PDF, scanned** | OCR word boxes → geometric reconstruction | APPROXIMATE | Requires OCR (§65); confidence per cell |
| Image of a table | OCR + reconstruction | APPROXIMATE | Screenshot case (§66.4) |
| ML layout models | Only on demand, only if a local model is installed | DEGRADED | **Reported ~8 GB RAM per worker for Docling's layout model — disqualifying as a default under NFR-005.** Optional, T-high+, demand-driven |

## 99.6 Requirements

| ID | Requirement |
|---|---|
| TBL-001 | All sources normalize into one Table IR; downstream consumers never branch on source format. |
| TBL-002 | Every cell retains its exact source location. |
| TBL-003 | Header detection is inferred with a confidence, never assumed to be the first row. |
| TBL-004 | Merged cells are preserved structurally. |
| TBL-005 | Column types are inferred and stored; the raw text is always retained alongside the typed value. |
| TBL-006 | Units are extracted from headers, number formats, unit rows and captions, and are attached to columns. |
| TBL-007 | XLSX formulas, dependencies and named ranges are preserved as relations (PAR-007). |
| TBL-008 | Numeric answers are computed over the IR and cite exact cells; models do not perform the arithmetic. |
| TBL-009 | Unit or type mismatch blocks an operation and is reported, never coerced silently. |
| TBL-010 | Cross-table joins require an explicit, user-visible key mapping. |
| TBL-011 | Table chunks repeat headers and caption; every table also emits a schema chunk. |
| TBL-012 | Headers, sheet names and named ranges receive exact-match boost in lexical search. |
| TBL-013 | `provenance_class` is recorded per table and per cell where it varies (OCR confidence). |
| TBL-014 | Low-confidence reconstructed tables are badged in the UI (CONV-004) and down-weighted in retrieval (CONV-005). |
| TBL-015 | Tables are exportable to CSV/XLSX with their provenance as a header comment or sidecar. |
| TBL-016 | Table edits go through structural patch tools with a reopen-and-recalc validator (§46.4), never text replacement. |
| TBL-017 | ML layout models are optional, hardware-gated and demand-driven; never on the default path. |
| TBL-018 | A table that fails reconstruction stays discoverable as text and is flagged, never dropped. |

---

# 100. Agent capability parity — `CAP`

The bar the user named: *"just like Claude CLI's and Codex and others."* Those systems converged on a small, well-understood tool set. Marrow must match it, and each tool must land inside the existing risk/reversibility/validator model rather than beside it.

## 100.1 Parity catalogue

| Capability | Coding-CLI norm | Marrow equivalent | Risk | Reversibility | Validator | Phase |
|---|---|---|---|---|---|---|
| Read file | `Read` with offset/limit | `filesystem.read` + IR-aware structural read | R1 | n/a | n/a | P1 |
| Write new file | `Write` | `filesystem.create` | R2 | `Reversible` | exists + MIME | P3 |
| Edit file | Exact-string replacement, **read-before-edit guard** | `filesystem.patch` + structural patch (§14.3) | R3 | `Reversible` | reparse + diff match | P3 |
| Glob | Path pattern match | `filesystem.search` (metadata index — **faster than walking**) | R0 | n/a | n/a | P1 |
| Grep | ripgrep | `text.search` over Tantivy + a literal-scan fallback for unindexed/binary | R0 | n/a | n/a | P1 |
| Shell | `Bash` with allow/ask/deny | §97 E1–E4 | R5 | `Irreversible` by default | per-runner | P3 (E1) → P6 (E3/E4) |
| Web fetch | `WebFetch` | `web.fetch` — content is `UNTRUSTED_EVIDENCE`, egress-policy gated | R5 | n/a | schema | P7 |
| Web search | `WebSearch` | `web.search` — off in local-only mode (SEC-015) | R5 | n/a | n/a | P7 |
| Plan / todo | Visible task list | §16.7 plan UI + recipe DAG (§97.3) | R0 | n/a | n/a | P3 |
| Subagent | Isolated context, filtered tools | Scoped runs with their own tool allowlist and budget (AGT-012) | inherits | inherits | inherits | P4 |
| Skills / saved workflows | Reusable instruction packs | Saved recipes in the Agents surface (§16.1) | inherits | inherits | inherits | P3 |
| MCP client/server | Both | §15 | per tool | per tool | per tool | P7 |
| Session resume | Persisted transcript | Persisted run + transaction state (§9.6) | — | — | — | P3 |
| Notebook edit | `.ipynb` cell-aware | Structural JSON patch (§14.3) | R3 | `Reversible` | reparse | P6 |
| Git | status/diff/commit/push | `git.*` (§15.1); push is `Irreversible` | R4/R5 | mixed | `git status` match | P5 |
| **Cross-format knowledge** | ✗ absent | Search/graph/timeline over every format | R0 | n/a | n/a | P1–P5 |
| **Provenance to location** | ✗ absent | Core (§61) | — | — | — | P2 |
| **Persistent knowledge across sessions** | ✗ absent | The product (§1) | — | — | — | P1+ |

## 100.2 What Marrow must adopt from that ecosystem

| Practice | Why | Requirement |
|---|---|---|
| **Read-before-edit guard** | Prevents blind edits from a stale mental model; complements §25 optimistic concurrency at a different layer (model vs filesystem) | CAP-001 |
| **Patch, never retype** | Retyping a file loses content silently and burns context | CAP-002 |
| **Deny-first with allow/ask/deny rules** | Proven interaction model for execution; maps onto §8.15's five outcomes | CAP-003 |
| **Layered independent blockers** | Any one layer can stop an action; no single bypass suffices | CAP-004 |
| **ripgrep-class literal search as a first-class path** | The index is not always current; users expect grep semantics to be exact and immediate | CAP-005 |
| **Visible plan/todo state** | Turns a long agent run from opaque to inspectable — §16.8 already wants this | CAP-006 |
| **Subagents as the same loop with a filtered tool set** | No new machinery; just scoped budget + tools + context | CAP-007 |
| **Structured output for tool arguments** | MOD-009 already requires validation; LLM-005 makes it a model-selection gate | CAP-008 |

## 100.3 Requirements

| ID | Requirement |
|---|---|
| CAP-001 | A mutation tool refuses to patch a file version the agent has not read in the current run. Complements, does not replace, the §25 stale-hash check. |
| CAP-002 | Edits are expressed as patches or structural operations. Whole-file rewrite is a distinct, higher-risk operation requiring explicit approval. |
| CAP-003 | Per-workspace allow / ask / deny rules for tools and executables, user-editable, defaulting to ask. |
| CAP-004 | Policy, allowlist, sandbox, resource limits and approval each independently block. Removing any one must not permit the action. |
| CAP-005 | Literal content search (exact, regex, case/word options) is available independently of index freshness, with a stated scope and time bound. |
| CAP-006 | Multi-step runs expose a live plan with per-step state, and a Stop that cancels queued steps and terminates cancellable workers (UX-010). |
| CAP-007 | Sub-runs get their own context, tool allowlist and budget; they cannot exceed the parent's policy or budget. |
| CAP-008 | Every tool argument set is schema-validated before policy evaluation; validation failure is a denial, not a retry loop. |
| CAP-009 | Runs are resumable after restart from persisted transaction state; a resumed run re-verifies targets before continuing. |
| CAP-010 | Tool results carry explicit trust labels; a tool result never widens authority (AGT-005). |
| CAP-011 | Token, step, wall-clock and cost budgets are enforced per run and shown live (AGT-012, BGT-002). |
| CAP-012 | Every capability in §100.1 is individually disableable by workspace policy and by enterprise policy (§22.1). |

---

# 101. Answer coverage matrix

The user's requirement: *"whatever the user can ask, we have an answer."* This is the audit of that claim. **Bold** rows are capabilities added by this part.

| # | Question archetype | Answered by | Phase | Needs LLM? |
|---|---|---|---|---|
| 1 | "Find the file called X" | Metadata/path index | P1 | No |
| 2 | "Which files did I touch yesterday?" | Metadata + timeline | P1 | No |
| 3 | "Where is the string `FOO_BAR`?" | Lexical / **literal scan (CAP-005)** | P1 | No |
| 4 | "What kind of files are in here, and how big?" | **File intelligence aggregate (§99.1)** | P1 | No |
| 5 | **"What do we know about this file?"** | **`FI` panel (§99.1)** | **P1** | **No** |
| 6 | "Who wrote this document?" | `META` OOXML/PDF author | P1 | No |
| 7 | "Where did this file come from?" | `META` download origin (META-004) | P1 | No |
| 8 | "Which files are duplicates?" | Content hash (FS-008) | P1 | No |
| 9 | "What's not indexed, and why?" | Index health + `FI` (FI-007) | P1 | No |
| 10 | "Where did we discuss X?" | Hybrid semantic retrieval | P2 | Optional |
| 11 | "What does this contract say about renewal?" | Retrieval + cited answer | P2 | Yes |
| 12 | "Summarise this document" | Summary hierarchy | P2 | Yes |
| 13 | "Is X mentioned anywhere?" | Lexical + semantic + abstention (§46.3) | P2 | Optional |
| 14 | **"What's the total in this invoice / this column?"** | **Table IR computation (TBL-008)** | **P2–P5** | **Only to phrase it** |
| 15 | **"Compare these two spreadsheets"** | **Cross-table with explicit key mapping (TBL-010)** | **P5** | **Partly** |
| 16 | **"Chart this for me"** | **G1 deterministic rendering (GEN-001)** | **P5** | **No** |
| 17 | "Change the config in these services" | Structural patch + diff + undo (Journey D) | P3 | Yes |
| 18 | "Create a file summarising X" | `filesystem.create`, marked `SELF` (§98.4) | P3 | Yes |
| 19 | **"Rename/reorganise these 200 files"** | **E1 recipe (§97.2)** | **P3** | **To plan it** |
| 20 | **"Extract every invoice total into a CSV"** | **E1 recipe + Table IR** | **P3** | **To plan it** |
| 21 | **"Run the tests"** | **E2 allowlisted runner (§97.4)** | **P3–P4** | **To plan it** |
| 22 | **"Run this script I wrote"** | **E3, gated on V2 sandbox (EXEC-015)** | **P6** | **No** |
| 23 | "Which services call AuthService?" | Code graph (Tree-sitter + LSP) | P4 | No |
| 24 | "How is Acme related to this project?" | Knowledge graph neighbourhood | P4 | Optional |
| 25 | "Are these two entities the same?" | Entity resolution + correction UX | P4 | Yes |
| 26 | "What changed in payment auth last month?" | Timeline + Git + versions | P5 | Optional |
| 27 | "What was I working on in March?" | Timeline + summaries | P5 | Yes |
| 28 | "What are the themes across my Q2 feedback?" | Community summaries (global mode) | P5 | Yes |
| 29 | "What does this screenshot say?" | OCR (§65) + screenshot routing | P5 | No |
| 30 | "Find the slide with the architecture diagram" | Image captions + OCR + doc structure | P5 | Yes (caption) |
| 31 | "What was said at 14:32 in that meeting?" | Subtitles / ASR with timestamp spans | P6 | No |
| 32 | **"Which model are you using, and can this machine run a bigger one?"** | **HW probe + model registry (HW-004, LLM-015)** | **P2** | **No** |

## 101.1 Honest gaps in the claim

| Question the product will **not** answer well | Why | Mitigation |
|---|---|---|
| Anything about files the user did not grant (WS-001) | Consent boundary, by design | Say so explicitly, offer to add the folder |
| Live SaaS state — "what's in my Jira right now?" | Not a connector platform (§82.3) | MCP client at P7, user-configured |
| Cloud-only placeholder file contents | TIER-005 never hydrates | Show the count and offer explicit hydration |
| Questions needing knowledge outside the corpus | Not a general assistant | Abstain (§46.3 step 7), or route to a model with that stated |
| Encrypted or DRM-protected content | Cannot parse | Metadata only, flagged (MAIL-009) |
| "What did I see on screen last Tuesday?" | Not a screen recorder (§82.1 C7) | Explicit non-goal |
| Anything requiring face or voice identification | IMG-015, AUD-009 | Explicit non-goal, stated in product |

**Rule:** every row above must produce a *specific* explanation, never a generic failure. §46.3 step 7 abstention plus SUP-001 cause-and-action errors are what make "we have an answer" true even when the answer is "no, and here is why."

---

# 102. Roadmap and cost delta

| Phase | Addition | Effort |
|---|---|---|
| **P1** | `HW` probe; `FI` panel; table extraction for XLSX/CSV/MD/HTML/DOCX (native formats only) | **+4–6 wk** |
| **P2** | `LLM` registry, sizing, runtime adapters (R1/R2), capability probes (LLM-005/006) | **+5–7 wk** |
| **P3** | `EXEC` E1 recipe engine; E2 runner + allowlist; `CAP` parity items (read-before-edit, literal search, plan UI, budgets) | **+8–11 wk** |
| **P5** | PDF ruled + borderless table reconstruction; `TBL` computation layer; G1 rendering | **+7–10 wk** |
| **P6** | E3/E4 behind V2 sandbox; G2 transforms; notebook structural edit | **+6–9 wk** |
| Post-P7 | G3 diffusion plugin (optional, may never ship) | +4–6 wk |
| All | §97.6 + §98.4 adversarial corpus additions; self-poisoning tests | **+2–3 wk** |

**Total added: 32–46 engineering weeks ≈ +5–7 months to the critical path.**

| Scenario | Part 3 estimate | Part 4 cost | With Part 5 |
|---|---|---|---|
| V1 (P0–P2) | 10–15 mo | ~$1.1M company cost | **12–17 mo, ~$1.3M** |
| Through P4 | 28–30 mo | ~$3.7M | **31–34 mo, ~$4.1M** |
| Full spec | 40–58 mo | ~$6.8M | **45–65 mo, ~$7.5M** |

| Size delta | |
|---|---|
| Embedded LLM runtime (mistral.rs or Candle) | +30–80 MB |
| Recipe engine (E1) | negligible |
| Sandbox (E3/E4, P6) | +5–15 MB |
| Chart/diagram rendering (G1) | +5–15 MB |
| Table reconstruction (native) | negligible; PDF borderless path rides the existing T3 sidecar |
| Local LLM weights | **Optional download, 1–9 GB.** Never bundled (PKG-003) |
| Diffusion weights (G3) | **Optional, 0.3–12 GB.** Never bundled |
| **Base installer delta** | **+40–110 MB** → PKG-001 revised to **≤ 500 MB** |

## 102.1 What to cut if the budget does not allow it

| Priority | Item | Reason |
|---|---|---|
| **Keep** | `HW` probe (P1) | Everything local-inference depends on it, and it is cheap |
| **Keep** | `FI` panel (P1) | Pure assembly of existing state; highest value per week in this part |
| **Keep** | Native-format table IR (P1) | XLSX/CSV/DOCX tables are deterministic and already parsed |
| **Keep** | E1 recipes (P3) | Most of the scripting value at none of the sandbox cost |
| **Keep** | `CAP` parity items (P3) | Cheap, and their absence reads as an unfinished agent |
| Defer | PDF borderless table reconstruction | Hard, per-document tuning, DEGRADED provenance anyway |
| Defer | E2 runners | Only after E1 proves what users actually script |
| Defer | G1 beyond charts | Diagrams and timelines are polish next to charts |
| Defer | E3/E4 | Blocked on V2 sandbox regardless |
| **Cut** | G3 diffusion | Lowest value, highest cost, only uncitable output in the system |
| **Cut** | ML table layout models on the default path | ~8 GB/worker is incompatible with NFR-005 |

---

# 103. New decisions and risks

## 103.1 Decisions

| # | Decision | Deadline | Recommendation |
|---|---|---|---|
| D31 | Embedded LLM runtime: mistral.rs vs Candle vs llama.cpp bindings | End P0 | Fold into D2; Candle wins on binary count if benchmarks are close (§94.2 A6) |
| D32 | Is Ollama detection-and-use a first-class path? | Start P2 | Yes — zero size, zero maintenance, real user base |
| D33 | Does E2 ship in P3 or wait for P4? | Mid P3 | Wait, unless E1 telemetry shows users needing external binaries |
| D34 | Default local model per capability tier | End P2 | Pick per tier; must pass LLM-005/006 probes, not benchmarks alone |
| D35 | G3 diffusion: ever? | Post-P7 | Default no; revisit only on clear demand and a clean commercial licence |
| D36 | PDF borderless tables: sidecar coordinate clustering vs on-demand ML layout | Start P5 | Sidecar first; ML strictly optional and hardware-gated |
| D37 | Recipe format: internal DAG only, or a documented public format? | Start P3 | Public and documented — saved recipes are a sharing surface and a moat |
| D38 | Do recipes get an MCP surface? | P7 | Yes, subject to MCP-004 capability approval |
| D39 | Structural notebook editing in P6, or drop it? | Start P6 | Ship it; `.ipynb` is JSON and the validator is trivial |
| D40 | Is literal search (CAP-005) exposed as a distinct UI mode? | P1 | Yes — grep semantics are what technical users reach for first |

## 103.2 Risks

| # | Risk | Prob | Impact | Mitigation |
|---|---|---|---|---|
| R33 | E1 recipe engine grows into an ad-hoc programming language | **High** | Medium | EXEC-003 is a hard constraint reviewed at every PR; if it needs loops, it needs E3 |
| R34 | Users read "scripts supported" as E4 and are disappointed | Medium | Medium | Name the tiers in the UI; §51-style honesty |
| R35 | Local model recommendation is wrong; model loads then OOMs | Medium | High | LLM-004 fallback ladder; KV cache counted in sizing; probe before offering |
| R36 | Model licence audit blocks the recommended default | Medium | High | LIC-001 checked in P0, not at ship |
| R37 | Table type/unit inference is confidently wrong | Medium | **High** | TBL-009 blocks on mismatch; raw text always retained (TBL-005); cells cited so the user can check |
| R38 | Borderless-PDF table reconstruction produces plausible-but-wrong numbers | **High** | **High** | APPROXIMATE provenance, UI badge, retrieval down-weight, and **never** used for TBL-008 computation without confirmation |
| R39 | Generated content re-enters the corpus as independent evidence | Medium | **Severe** | §98.4 `SELF` origin flag, enforced at schema level, with an adversarial test |
| R40 | Execution sandbox slips again, as in §51 | **High** | Medium | E1 carries the value; E3/E4 are explicitly gated (EXEC-015) |
| R41 | `FI` panel becomes a slow join across every table | Medium | Medium | Read model built from indexed canonical state, budgeted like a query (NFR-009) |
| R42 | Parity chase pulls the roadmap toward being a coding agent | Medium | High | §100.1's last three rows are the differentiators; parity items are table stakes, not strategy |

---

# 104. Summary of Part 5

| Area | Before | After |
|---|---|---|
| Script support | "Shell disabled in V1" (§51), nothing else | 5-tier `EXEC` model; **E1 recipes ship in P3 with no sandbox dependency**; E3/E4 gated on V2 posture |
| Local LLM | Adapters named, no selection logic | `HW` probe (10 reqs) + `LLM` registry (15 reqs); sizing arithmetic; 5 capability tiers; runtime selection tree that prefers an already-installed runtime |
| Image generation | Not mentioned | `GEN` (14 reqs); G1 rendering prioritised over G3 diffusion; G3 optional, never bundled, licence-gated |
| **Self-poisoning** | **Not identified** | **§98.4 — all agent-written and generated content marked `SELF` and excluded from evidence authority** |
| Tables | Chunking edge case | `TBL` (18 reqs); unified Table IR; **numeric answers computed, not read**; per-source provenance tiers |
| File intelligence | Data existed, was never assembled | `FI` (8 reqs) — one panel, every fact, every authority class |
| Agent parity | Tools sketched | `CAP` (12 reqs) + full parity catalogue mapped to risk/reversibility/validator |
| Answer coverage | 7 query modes | 32 archetypes with phase and LLM-dependency, plus 7 honest gaps |
| Prior art | None | 13 systems reviewed; 6 conclusions that change the spec |
| Timeline | 40–58 mo | **45–65 mo**; V1 **12–17 mo**, ~$1.3M |

## 104.1 New requirement blocks

| Prefix | Topic | Count | Phase |
|---|---|---|---|
| `HW` | Hardware capability probe | 10 | **P1** |
| `LLM` | Local model support and routing | 15 | P2 |
| `EXEC` | Script and command execution | 20 | P3 (E1) → P6 (E3/E4) |
| `GEN` | Generative media | 14 | P5 |
| `FI` | File intelligence panel | 8 | **P1** |
| `TBL` | Table understanding | 18 | P1 (native) → P5 (PDF) |
| `CAP` | Agent capability parity | 12 | P3 |
| **Total added** | | **97** | |

Running requirement total across Parts 1–5: **~520**.

## 104.2 Sources consulted

- Spacedrive — https://github.com/spacedriveapp/spacedrive
- Khoj — https://clawbot.ai/wiki/ai-processing/khoj-open-source-ai-knowledge-management.html
- Onyx (ex-Danswer) — https://github.com/onyx-dot-app/onyx
- Agent sandboxing comparison (Codex Seatbelt / OpenHands / Docker) — https://codex.danielvaughan.com/2026/04/24/agent-sandbox-comparison-codex-seatbelt-openshell-docker-sbx/
- How Claude Code and Codex sandbox untrusted code — https://medium.com/@Koukyosyumei/how-claude-code-and-codex-sandbox-untrusted-code-ba39b493046a
- What I learned reading 15 AI agent codebases — https://dev.to/neuzhou/what-i-learned-reading-15-ai-agent-codebases-1ggl
- mistral.rs — https://github.com/ericlbuehler/mistral.rs
- Candle Stable Diffusion example — https://github.com/huggingface/candle/tree/main/candle-examples/examples/stable-diffusion
- `candle_transformers::models::stable_diffusion` — https://docs.rs/candle-transformers/latest/candle_transformers/models/stable_diffusion/
- stable-diffusion.cpp — https://sourceforge.net/projects/stable-diffusion-cpp.mirror/
- Local LLM hardware requirements — https://overchat.ai/ai-hub/llm-hardware-requirements
- Ollama VRAM requirements guide — https://localllm.in/blog/ollama-vram-requirements-for-local-llms
- PDF table extraction comparison — https://invoicedataextraction.com/blog/python-pdf-table-extraction-invoices
- Open-source PDF-to-JSON extraction models 2026 — https://www.huuphan.com/2026/07/blog-post_15.html
- Camelot — https://camelot-py.readthedocs.io/
- Why AI agents prefer the CLI (tool conventions) — https://www.firecrawl.dev/blog/why-is-cli
