# Marrow — Master Specification, Part 7

## Solo / Self-Use / Open-Source Re-Scope

**Status:** Addendum to Parts 1–6. **Supersedes Part 4 in its entirety** and amends Parts 2, 3 and 5 where noted.
**Date:** 30 August 2026
**Numbering:** Continues from §122 of Part 6
**Format:** Tables and points only

---

# 123. The change

| Before | Now |
|---|---|
| Commercial desktop product, multiple SKUs, unknown users | **One user: you. One machine: yours.** |
| Sold, licensed, supported | **Open source, not sold** |
| Must run on anything from a 4 GB laptop up | **Decent PC.** One hardware target |
| 3 platforms | **One platform** (pick it; see §127) |
| 45–65 months, ~$7.5M, 4–9 engineers | **Solo, part-time, no budget** |

## 123.1 Why this is not just "delete the business sections"

Three consequences change the *architecture*, not only the plan:

| # | Consequence | Effect |
|---|---|---|
| **1** | **You already have agent front-ends.** Claude Code, Codex, Cursor are on your machine and are better at the agent loop than a solo build will be for years. | **Marrow does not need its own agent runtime, model gateway, approval UX or chat UI.** It needs to be an excellent index with an MCP server. That deletes roughly 60% of the spec — see §130. |
| **2** | **You are the operator, the threat model changes shape.** No untrusted second user, no fleet, no liability. But you still open hostile PDFs, and anything you wire to a write-capable agent has your whole home directory as its blast radius. | Some defences drop entirely; a small set gets *more* important, not less (§126). |
| **3** | **GPL is now usable.** LIC-005 denied GPL/AGPL because of commercial redistribution. That constraint is gone. | Better libraries become available — GPL ffmpeg builds, GPL-licensed parsers and table tools (§128). |

---

# 124. Out of scope

| Area | Sections | Status | Reason |
|---|---|---|---|
| SKUs, pricing, unit economics | §79, §80 | **Deleted** | Not selling |
| Entitlement / licensing enforcement | §81, `ENT` (12) | **Deleted** | No licence to enforce |
| Competitive analysis, positioning | §82 | **Parked** | §94 prior art is still useful for build decisions; the positioning is not |
| GTM, distribution channels, launch gates | §83 | **Deleted** | No launch. Keep §83.3's *technical* gates as personal quality bars |
| Support tiers, severity SLAs, deflection | §84, `SUP` (12) | **Deleted** — except SUP-001 | You are the only support tier. But good error messages still matter, because you debug them |
| Security incident response, disclosure, bounty | §85, `SIR` (10) | **Deleted** — except SIR-008 | No users to notify. Keep dependency CVE awareness, loosely |
| Compliance, SOC 2, ISO, EU AI Act, export control | §86, `CMP` (15) | **Deleted** | No vendor-side services, no customers, no distribution of binaries |
| DPAs, subprocessors | §87, `DPA` (10) | **Deleted** | No subprocessors. BYO-key means your traffic goes to your provider under your account |
| Legal artifacts (EULA, ToS, privacy policy, VPAT) | §88 | **Deleted** — except a project LICENSE + NOTICE | §128 |
| Company cost model | §89 | **Deleted** | Replaced by §131: your time |
| Multi-user isolation | §78, `MULTI` (15) | **Reduced to 3** | MULTI-002 (per-user data dir), MULTI-007 (never index onto a network share), MULTI-011 (never require admin). The rest is unnecessary |
| Telemetry | `TEL` (6) | **Deleted** | Nothing phones home. Removes the opt-in flow, the scrubber and its tests |
| Accessibility mandate | `A11Y` (6) | **Optional** | Keep only what you personally need |
| Internationalization | `I18N` (9) | **Reduced to 2** | I18N-001 (language detection, improves tokenization) and **I18N-009 (Unicode NFC/NFD — a correctness bug, not a locale feature)** |
| Packaging budgets, signing, delta updates, installers | `PKG` (12) | **Reduced to 3** | PKG-009 (no update mid-transaction), PKG-011 (verify model downloads), PKG-012 (clean uninstall). Size budgets are gone — bundle whatever you want |
| Enterprise policy, private inference, audit export | §22 | **Deleted** | — |
| Air-gapped SKU | §79.1 | **Deleted** | Local-only mode remains as a switch, not a product |
| Sandbox tiers E2–E4 hardening | §97.5, §51 V2 | **Deleted as specified** | §129 replaces it |
| Email, video, audio subsystems | `MAIL`, `VID`, `AUD` (32) | **Deferred indefinitely** | Build if you personally need them, not because the spec lists them |
| Image generation | `GEN` G3 | **Deleted** | Part 5 §98.2 already recommended against it. Nothing has changed except that now there is no one to demand it |

**Requirements removed or reduced: ~140.** Running total drops from ~520 to **~380**, and the genuinely-must-build subset is far smaller again (§133).

---

# 125. What gets easier

| Win | Detail |
|---|---|
| **No approval UX** | You can run with a permissive policy by default and tighten only where you have been burned. The elaborate §16.7 plan UI is not needed if an existing agent front-end is doing the asking |
| **No sandbox project** | §129. This alone was 8–14 weeks of specialist work with a high slip risk (R40) |
| **One platform** | One watcher path, one OCR engine, one path-canonicalization dialect, one build. The §24 failure matrix shrinks by two thirds |
| **One hardware tier** | Drop T-min/T-low degradation entirely (§127). No "what if they have 4 GB" branches |
| **GPL available** | §128 |
| **No installer** | `cargo build --release` and a shell alias is a valid distribution strategy for one user |
| **No migrations discipline… almost** | You can afford to blow away derived indexes. You **cannot** afford to lose corrections — keep the backup rule (§126) |
| **No telemetry plumbing** | Local metrics via `tracing` to a file. Done |
| **Model licensing relaxes** | LIC-003 forbade bundling non-commercial models. You are not distributing binaries, so you can *use* anything locally. Only re-distribution would trigger it |

---

# 126. What must **not** be relaxed

Solo use removes the audience, not the failure modes. These stay because they protect *your* data on *your* machine.

| # | Keep | Why it still bites |
|---|---|---|
| 1 | **Injection defence** (SEC-003/004, ADR-007, §114 envelope) | You download hostile PDFs and clone unknown repos. If this feeds a write-capable agent, a poisoned README targets your home directory. **Highest-value defence in the whole spec for a solo user** |
| 2 | **Path canonicalization + symlink escape** (SEC-001/002, §6.3) | A symlink in a cloned repo pointing at `~/.ssh` is not a hypothetical |
| 3 | **Never hydrate cloud placeholders** (TIER-001..005) | OneDrive/iCloud/Dropbox on your own machine will happily download 400 GB. This was Part 2's highest-severity finding and it is entirely about your bandwidth and disk |
| 4 | **Stale-write check** (§25) | You will have the file open in your editor while the agent edits it |
| 5 | **Transaction snapshots + undo** (§47.4) | The only backstop between a bad patch and losing work |
| 6 | **Backup before migration** (`VACUUM INTO`, §107) | Derived indexes are disposable. Your corrections and workspace policy are not |
| 7 | **Path ≠ identity, stable file IDs, path history** (FS-005/006) | Expensive to retrofit at any team size, including one |
| 8 | **Provenance to exact location** (§61) | The entire reason to build this rather than use grep + an LLM |
| 9 | **Idempotent, resumable jobs** (§20.2, NFR-003) | You will kill the process constantly during development |
| 10 | **Self-poisoning rule** (§98.4, ADR-013) | If the agent writes notes into an indexed folder, it will cite itself back to you |
| 11 | **Watcher is a hint; reconcile anyway** (§2.6) | Missed events silently produce wrong answers, which is worse than no answers |
| 12 | **Authority classes + confidence** (§11.2) | Without it you cannot tell what the system knows from what it guessed, and you will stop trusting it |
| 13 | **Search works with no LLM** (ADR-010) | Your fallback when everything else is half-built |
| 14 | I18N-009 Unicode NFC/NFD | macOS NFD vs NFC creates duplicate file identities. A correctness bug wearing an i18n costume |

**Rule of thumb:** drop anything that protects *other people*. Keep everything that protects *your files, your bandwidth, and your ability to trust the output*.

---

# 127. Hardware assumption

Replaces §95.3's five-tier matrix.

| Assume | Value |
|---|---|
| Target tier | **T-mid to T-high** (§95.3): 16–32 GB RAM, modern multi-core CPU, optional GPU |
| Degradation branches | **None.** If it does not run on your machine, that is a bug, not a tier |
| Local LLM | 7–14B @ Q4 comfortably. 30B+ if you have 32 GB unified or 24 GB VRAM |
| Local embeddings | Fine on CPU; accelerated if a GPU is present |
| OCR | Platform-native (§65.1) — free and good on macOS/Windows; Tesseract on Linux |
| `HW` block | **Reduced to 2 requirements:** HW-001 (probe once, cache it) and HW-004 (show what it found). Everything else was about serving unknown machines |
| Corpus scale | Size the P0 benchmark to *your actual corpus*, not 100k synthetic files. Measure your real home directory first — it is probably smaller and weirder than the spec assumes |

**Do this before writing code:** count your files by extension and size, and find how many live in cloud-sync folders. That single measurement will change your parser priorities more than any table in Parts 1–6.

---

# 128. Licence strategy

| Decision | Recommendation |
|---|---|
| **Project licence** | **MIT or Apache-2.0** if you want maximum optionality later (including commercialising, which your wording leaves open). **GPL-3.0** if you want to use GPL dependencies freely and do not mind the copyleft |
| Which to pick | If genuinely undecided: **Apache-2.0**. It keeps every future door open, including relicensing your own code, and it grants patent rights that MIT does not |
| Consequence for dependencies | Under Apache-2.0/MIT, **avoid linking GPL** in anything you publish binaries for. Since you are publishing *source* and building locally, this is mostly a theoretical constraint — but it becomes real the moment you ship a binary to anyone |
| **The practical unlock** | Even under Apache-2.0, GPL tools invoked as **separate processes** (ffmpeg via CLI, a Python sidecar) are fine. Only *linking* is the problem. Prefer subprocess invocation for GPL tools and the question evaporates |
| Model licences | You may run anything locally. LIC-003's bundling ban only applies if you redistribute weights — do not put weights in the repo |
| NOTICE file | Generate one from `cargo about` or similar. Cheap, and correct |
| `LIC` block | Reduced from 7 to 2: LIC-002 (record model licence + SHA) and LIC-005 (know your dependency licences, no CI gate needed) |

---

# 129. Execution posture — the big simplification

Part 5 §97 built a five-tier execution model because arbitrary shell for *unknown users* requires a sandbox nobody has time to build (§51, R40). For solo use that reasoning inverts.

| Question | Commercial answer | Solo answer |
|---|---|---|
| Who is protected from the shell? | Unknown users from a malicious model | **Nobody — you are already the operator.** You run arbitrary shell all day |
| Is a sandbox worth ~17k lines? | Yes, eventually | **No. Never build it.** |
| So execution is…? | Tiered E0–E4 with E3/E4 gated for years | **E4 available from the start, behind a confirmation prompt** |

## 129.1 Revised model

| Tier | Solo status |
|---|---|
| **E1 recipes** | **Still build this** — not for safety, for *repeatability*. A saved recipe that re-resolves targets, previews diffs and rolls back is genuinely better than a shell script for file operations |
| E2 allowlisted binaries | Merge into E4. Not worth a separate tier |
| E3/E4 shell | **Available immediately.** Structured argv (EXEC-007) and env allowlist (EXEC-009) are kept — not as security controls but because they eliminate a whole class of quoting bugs |
| Sandbox (§97.5) | **Deleted** |

## 129.2 What survives from `EXEC`, and why

| Keep | New reason (not security) |
|---|---|
| EXEC-004 (resolve to concrete actions before executing) | You want to see what it will do before it does it |
| EXEC-005 (reversibility = weakest step) | You want undo |
| EXEC-006 (re-resolve targets on re-run) | A saved recipe run against stale paths is a footgun |
| EXEC-007 (structured argv, no shell string) | Quoting bugs, not injection |
| EXEC-009 (env allowlist) | Stops your provider keys leaking into subprocess logs |
| EXEC-011 (resource limits) | Stops a runaway build eating your machine |
| EXEC-012 (validator per runner) | You want to know if the edit actually parsed |
| EXEC-017 (timeline of what ran) | You will want to know what it did last Tuesday |
| EXEC-018 (global stop) | Obvious |
| **EXEC-020 (adversarial corpus)** | **Keep.** §126 #1 — the injection path to execution is the one real risk |

`EXEC` drops from 20 requirements to **10**.

---

# 130. Architecture simplification — the MCP-first inversion

This is the most important section in this part.

## 130.1 The insight

Parts 1–6 assume Marrow must own the entire stack: index → retrieval → context → model → agent loop → approval UI → chat. That is correct for a product. For one user who already runs Claude Code, it is **building a worse copy of tools you already have.**

```text
  Spec'd architecture                     Solo architecture
  -------------------                     -----------------
  [ Chat UI ]                             [ Claude Code / Codex / Cursor ]   ← already exists
  [ Approval UX ]                                    |
  [ Agent runtime ]                                  | MCP
  [ Model gateway ]                                  v
  [ Context builder ]                     [ Marrow MCP server ]
  [ Retrieval ]         ==>               [ Retrieval ]
  [ Knowledge substrate ]                 [ Knowledge substrate ]   ← the part only you can build
  [ Ingestion ]                           [ Ingestion ]
```

## 130.2 What this deletes from the critical path

| Subsystem | Spec'd effort | Solo status |
|---|---|---|
| Chat / Ask UI (§16.4) | Months | **Delete.** Your agent front-end is the chat |
| Approval UX, plan UI, live action UI (§16.7, §16.8) | Months | **Delete.** Claude Code already asks before it runs things |
| Agent runtime: planner, tool selector, step executor, budget guard (§8.16) | Months | **Delete.** That is the front-end's job |
| Model gateway + routing (§8.14, `MOD`) | Weeks–months | **Delete for v1.** Needed only when Marrow generates text itself — i.e. for summaries and Tier C, which are late |
| Context builder (§12.3, §114) | Weeks | **Reduce.** MCP tool results are the context; the envelope discipline (§114) still applies to what you return |
| Tauri desktop shell (§8.1) | Months | **Defer.** A CLI plus the MCP server covers most of it. Build a UI when you miss one |
| MCP server (§15.1) | Was P7 | **Promote to P1.** It is now the primary interface |
| Knowledge substrate: scan, parse, IR, index, provenance, tables, graph | — | **This is the whole project.** It is also the part no existing tool does well |

## 130.3 Process architecture

| Spec (§7.1) | Solo |
|---|---|
| Desktop app + daemon + index workers + inference worker + sandboxed tool worker | **One binary.** In-process async workers, plus **one subprocess for parsers** |
| Why keep the parser subprocess | A malformed PDF killing your indexer is annoying enough to be worth the one boundary (NFR-001). Everything else can share a process |
| IPC | **Deferred.** No daemon split means no IPC contract, no peer-credential auth (§49), no protocol versioning. Reinstate only if you later split |
| MCP transport | stdio for local agent front-ends. Simplest thing that works |

**Part 6 §110 (IPC contract) is therefore deferred, not deleted** — it becomes relevant again only if you split the daemon out.

---

# 131. Build plan

Milestones, not phases. Each one ends at something you actually use. **Ranges are solo: (part-time / focused full-time).**

| # | Milestone | Contents | Effort | Done when |
|---|---|---|---|---|
| **M0** | **Measure** | Count your own corpus by type, size, location, cloud-sync status. Spike: `ignore` walk + `blake3` + SQLite insert on your real home dir | 1 wk / 2 days | You know your actual numbers, not the spec's |
| **M1** | **Index + query** | Workspaces, scan, `notify` watcher + reconciliation, SQLite schema (Part 6 §106, trimmed), parsers for text/Markdown/code/JSON/CSV, Tantivy, `META` extraction, a CLI that searches | 6–10 wk / 3–4 wk | You use it instead of `grep`/Spotlight |
| **M2** | **MCP server** | Expose `search`, `read`, `file.intelligence`, `stat` over MCP stdio. Point Claude Code at it | **1–2 wk / 3–5 days** | **Your coding agent can search your whole corpus.** Highest value-per-hour in the entire plan |
| **M3** | **PDF + tables** | PDFium text + page provenance; native table IR for CSV/XLSX/Markdown/HTML; `TBL` compute; expose via MCP | 4–7 wk / 2–3 wk | You can ask about spreadsheets and get cited numbers |
| **M4** | **Semantic** | Chunking (§112), local embeddings, embedded vector store, RRF hybrid (§113) | 4–6 wk / 2–3 wk | Conceptual search beats lexical on your own queries |
| **M5** | **Write tools** | `filesystem.patch` with stale-check, snapshots, undo, validators; E1 recipes | 4–6 wk / 2–3 wk | You let an agent edit through Marrow rather than directly |
| **M6** | **Timeline** | File events, Git integration, "what changed" queries | 3–4 wk / 1.5–2 wk | You can reconstruct last month |
| **M7+** | **Optional** | Graph + entities (highest risk, least certain payoff — §55 R2), OCR, local LLM for summaries, a UI, media | Open-ended | Only if you miss it |

| Cumulative to | Part-time | Full-time |
|---|---|---|
| M2 — **useful daily** | **7–12 weeks** | **4–5 weeks** |
| M4 — semantic search working | 15–25 weeks | 8–12 weeks |
| M6 — the substantive product | 22–35 weeks | 11–17 weeks |

**Compare to Parts 1–6:** 45–65 months. The difference is not optimism — it is §124 (nothing sold), §130 (no agent layer), and §127 (one machine).

## 131.1 Stop rules

| Rule |
|---|
| **Ship M2 before doing anything clever.** An index you query from Claude Code every day will teach you more about what to build next than any amount of planning |
| If a milestone exceeds 2× its estimate, cut its scope rather than extending it |
| Build the knowledge graph (M7) **only** after M6, and only if you can name three questions you actually asked that needed it. §55 R2 rates graph quality as the highest-probability, highest-impact risk in the spec, and it is the easiest thing to build badly |
| Do not build a UI until the CLI genuinely annoys you |
| Do not build multi-format parsers speculatively. Add a parser the week you hit a file you wanted and could not read |

---

# 132. Revised requirement status

| Block | Was | Now | Note |
|---|---|---|---|
| WS Workspace | 12 | 6 | Drop enterprise, pause/resume nuance |
| FS Filesystem | 16 | **16** | Keep all — this is the core |
| PAR Parsing | 14 | 10 | Drop archive/OCR-adjacent until needed |
| CHK Chunking | 8 | **8** | Keep — cheap and load-bearing |
| IDX Lexical | 7 | **7** | Keep |
| EMB Embeddings | 10 | 7 | Drop provider-policy items |
| KG Graph | 14 | 14 | Deferred to M7, unchanged if built |
| TMP Temporal | 6 | **6** | Keep |
| RET Retrieval | 12 | 10 | Drop policy-driven items |
| MOD Model gateway | 9 | 3 | Only what M7 summaries need |
| AGT Agent | 15 | 8 | Front-end owns most of it |
| MCP | 12 | **8** | **Promoted to M2.** Drop enterprise capability-approval items |
| SEC Security | 18 | **12** | Keep every §126 item; drop enterprise/DLP |
| UX | 16 | 4 | CLI-first |
| NFR | 12 | **10** | Keep reliability, relax perf targets to "fast enough on your box" |
| TIER Cloud placeholders | 12 | **8** | §126 #3 — keep detection and never-hydrate |
| WATCH | 10 | **8** | Keep; degraded-mode honesty matters when *you* are debugging |
| I18N | 9 | 2 | I18N-001, I18N-009 |
| PKG | 12 | 3 | §124 |
| LIC | 7 | 2 | §128 |
| EVAL | 10 | 4 | Golden query set on your own corpus; adversarial corpus; correction rate |
| SYNC | 7 | 3 | Export/import + ULID + `origin_device_id` — cheap future-proofing |
| CONV | 12 | 6 | T3 sidecar only if you hit formats you need |
| OCR / IMG | 29 | 8 | Screenshot OCR is the high-value case; skip captioning |
| META | 10 | **10** | Cheapest knowledge in the system (§69.2). Keep all |
| HW | 10 | 2 | §127 |
| LLM | 15 | 6 | Local model for summaries only, late |
| EXEC | 20 | **10** | §129.2 |
| GEN | 14 | 3 | G1 chart rendering only, if ever |
| FI File intelligence | 8 | **8** | Keep — it is mostly free once the schema exists |
| TBL Tables | 18 | **12** | Keep native formats; defer borderless-PDF reconstruction |
| CAP Agent parity | 12 | 5 | Front-end provides most of it; keep CAP-001, 002, 005, 008, 010 |
| MULTI / ENT / SUP / SIR / CMP / DPA / TEL / A11Y / VID / AUD / MAIL | 128 | **3** | §124 |
| **Total** | **~520** | **~245** | Of which ~120 are M1–M4 |

---

# 133. Open-source project hygiene

Minimal, not ceremonial. Everything here is for future-you, not for contributors you do not have yet.

| Item | Do | Skip |
|---|---|---|
| `LICENSE` | Apache-2.0 (§128) | — |
| `README` | What it is, what it is not, how to run it, current milestone | Badges, roadmaps, contribution ladders |
| `NOTICE` | Generated dependency attributions | — |
| `SECURITY.md` | One paragraph: "personal project, no SLA, report issues as issues" | Disclosure policy, bounty (SIR — deleted) |
| CI | `cargo test`, `clippy`, `fmt`. **Plus the adversarial corpus** (§116.2) | SBOM, licence gates, size budgets, drift checks, perf gates |
| Tests | Unit + the §116.3 invariant tests + adversarial corpus | Soak, platform matrix, upgrade matrix, accessibility |
| Benchmarks | One script against your own corpus, run when you suspect a regression | Nightly pinned-hardware suite |
| Docs | These files, plus an `ARCHITECTURE.md` that stays under 200 lines | Anything requiring maintenance you will not do |
| Issues | Use them as your own backlog | Templates, labels, triage process |
| Releases | Git tags | Installers, signing, notarization, delta updates |
| **Warning in README** | **"Indexes files you point it at. Do not point it at anything you would not want an LLM to read."** | — |

---

# 134. Revised decisions

Most of D1–D40 evaporate. What remains, with solo-appropriate answers:

| # | Decision | Solo answer |
|---|---|---|
| D1 | Vector store | **Defer to M4.** Start with brute-force cosine over a few hundred thousand vectors — it is fast enough and has no dependency. Add LanceDB only when it hurts |
| D2 / D31 | Embedding + LLM runtime | **Candle** if you want one crate for embeddings and generation. **Or skip entirely at M4** by calling an already-installed Ollama |
| D3 | Tantivy vs FTS5 | **Tantivy.** You get BM25 and field-aware search without building it |
| D4 | PDF engine | **PDFium.** The pure-Rust options are not there for provenance |
| D5 | First platform | **Your daily driver.** Do not abstract for others |
| D13 | OCR | **Platform-native.** Free, and better than bundled Tesseract on macOS/Windows |
| D17 | GPS extraction | **Off.** It is your own photo library; turn it on deliberately if you want it |
| D19–D30 | Commercial / compliance | **Deleted** |
| D33 | E2 tier | **Deleted** — merged into E4 (§129) |
| D37 | Recipe format public? | **Yes**, and keep it simple JSON. You will hand-edit it |
| **D41** *(new)* | Project licence | **Apache-2.0** (§128) |
| **D42** *(new)* | Build a UI at all? | **Not until after M6.** CLI + MCP + your existing agent front-end covers it |
| **D43** *(new)* | Build the knowledge graph at all? | **Undecided by design.** Gate it on §131.1's three-questions test |
| **D44** *(new)* | Single binary or daemon split? | **Single binary + parser subprocess** (§130.3). Revisit only if you need background indexing while the CLI is closed |

## 134.1 Revised risks

| # | Risk | Prob | Impact | Mitigation |
|---|---|---|---|---|
| **S1** | **Scope collapse into an unfinished everything** — the spec is 7 parts long and you are one person | **Very high** | **Severe** | §131 milestones with stop rules. M2 is the forcing function |
| S2 | Building the graph too early and losing months | High | High | §131.1 gate |
| S3 | Injection reaching a write tool once wired to an agent | Low | **Severe** | §126 #1. This is the one security investment that pays for itself |
| S4 | Placeholder hydration eats your bandwidth | Medium | High | TIER-001..005, in M1 not later |
| S5 | Parser work expands without limit | High | Medium | §131.1 — add a parser the week you need it, never before |
| S6 | Losing corrections to a schema change | Medium | High | Backup before migration (§126 #6) |
| S7 | Interest fades before M2 | **High** | Severe | Keep M2 at 1–2 weeks. Nothing else matters if this slips |

Risks R1–R42 from Parts 2–5 still apply *technically* where the corresponding subsystem is built; the commercial and fleet-scale ones (R23–R32) are void.

---

# 135. Summary

| Area | Parts 1–6 | Part 7 |
|---|---|---|
| Audience | Unknown users, multiple SKUs | **You** |
| Distribution | Signed installers, 3 platforms, stores | **`cargo build`, one platform, public repo** |
| Requirements | ~520 | **~245**, of which ~120 are the first four milestones |
| Architecture | Desktop app + daemon + 4 worker types + MCP at P7 | **One binary + parser subprocess, MCP at M2** |
| Agent layer | Built from scratch | **Delegated to Claude Code / Codex via MCP** |
| Execution | 5 tiers, sandbox project, shell deferred to P6 | **Shell available immediately; no sandbox; keep E1 recipes for repeatability** |
| Security | Full threat model, IR process, compliance | **14 defences that protect your own files** (§126) |
| Licensing | Commercial redistribution constraints, no GPL | **Apache-2.0; GPL usable via subprocess** |
| Time to daily usefulness | ~12–17 months to V1 | **7–12 weeks part-time to M2** |
| Full scope | 45–65 months, ~$7.5M | **~6–9 months part-time to M6** |

## 135.1 What to do next, in order

1. **§127's measurement.** Count your own corpus. One afternoon.
2. **M0 spike.** `ignore` + `blake3` + SQLite on your real home directory. Find out where it is slow and where it is wrong.
3. **Trim Part 6 §106's DDL** to the M1 tables: `files`, `file_paths`, `file_versions`, `parse_results`, `ir_nodes`, `chunks`, `jobs`, `workspaces`, `workspace_roots`. Leave the graph, action and media tables out until M5/M7 — the columns can be added, but carrying 40 unused tables will slow you down.
4. **M1, then M2.** Do not detour.

## 135.2 The one thing to keep from the commercial spec

Provenance. Everything else in Parts 1–6 is negotiable at this scale; the ability to click an answer and land on the exact page, cell or line is the only reason to build this instead of piping `ripgrep` into an LLM. It is also nearly free if the schema carries `source_span` from day one, and nearly impossible to add later.
