# LKAR — Master Specification, Part 2

## Gap Closure, Verification Subsystem, Cost Model, Revised Delivery Plan

**Status:** Addendum to *Local Knowledge & Agent Runtime (LKAR) Master Specification*
**Date:** 28 August 2026
**Numbering:** Continues from §43 of Part 1
**Format:** Tables and points only

---

# 44. Product definition (restated, single line per item)

| Field | Value |
|---|---|
| Product | Desktop app. Local knowledge runtime + agent. |
| Scan scope | User-selected folders only. Never whole OS. Never silent. |
| Core loop | Scan → parse → index → graph → retrieve → answer → act → verify |
| Intelligence | Replaceable. Local / private / cloud. |
| Durable asset | Knowledge graph + provenance + corrections. Not the model. |
| Agent power | Read, modify, create, execute — all gated, all verified, all reversible-or-declared |
| Platforms | V1: one platform. V2: three. |
| Data default | Stays on device. |

---

# 45. Missing requirement catalogue (new ID blocks)

## 45.1 Cloud-backed and tiered storage — `TIER`

**Reason:** Highest-severity gap in Part 1. Indexing dehydrated placeholder files silently downloads the user's entire cloud drive.

| ID | Requirement |
|---|---|
| TIER-001 | Detect placeholder / dehydrated files before any read. |
| TIER-002 | Windows: check `FILE_ATTRIBUTE_RECALL_ON_DATA_ACCESS`, `FILE_ATTRIBUTE_RECALL_ON_OPEN`, `FILE_ATTRIBUTE_OFFLINE`. |
| TIER-003 | macOS: check dataless flag (`SF_DATALESS`) and `.icloud` stub files. |
| TIER-004 | Linux: treat known sync-client mount points as tiered by config. |
| TIER-005 | Default policy: **index metadata only, never hydrate**. |
| TIER-006 | Hydration requires explicit per-workspace opt-in with size estimate shown. |
| TIER-007 | Hydration must be rate-limited and cancellable. |
| TIER-008 | UI must show count of "cloud-only, not indexed" files per workspace. |
| TIER-009 | Placeholder files that later hydrate naturally shall be picked up by reconciliation. |
| TIER-010 | Metered-connection detection shall suspend hydration. |
| TIER-011 | Never hydrate on battery unless user explicitly overrides. |
| TIER-012 | Provider detection (OneDrive / iCloud / Dropbox / Google Drive) recorded on the root. |

## 45.2 Filesystem watcher limits — `WATCH`

| ID | Requirement |
|---|---|
| WATCH-001 | Linux: probe `fs.inotify.max_user_watches` at startup. |
| WATCH-002 | If watch budget < directory count, degrade to selective watching. |
| WATCH-003 | Selective watching: watch top N directories by recent-activity score. |
| WATCH-004 | Unwatched subtrees fall back to shortened reconciliation interval. |
| WATCH-005 | Windows: handle `ReadDirectoryChangesW` buffer overflow → mark root stale → force reconciliation. |
| WATCH-006 | macOS: persist FSEvents event ID; on restart, replay from last known ID. |
| WATCH-007 | Network / SMB / NFS roots: watchers marked unreliable; reconciliation is primary. |
| WATCH-008 | UI must expose per-root watcher health: `Live` / `Degraded` / `Poll-only`. |
| WATCH-009 | Never silently drop to poll-only without surfacing it. |
| WATCH-010 | Reconciliation interval adaptive: 5 min (degraded) → 6 h (healthy, cold). |

## 45.3 Email and message ingestion — `MAIL`

**Reason:** Absent from Part 1. Core persona's knowledge is largely in mail.

| ID | Requirement | Phase |
|---|---|---|
| MAIL-001 | Support `.eml`, `.mbox` as file-level sources. | V1.5 |
| MAIL-002 | Support `.pst` / `.ost` read-only. | V2 |
| MAIL-003 | Each message = one logical unit with stable ID (Message-ID header). | V1.5 |
| MAIL-004 | Thread reconstruction via `In-Reply-To` / `References`. | V1.5 |
| MAIL-005 | Attachments parsed under normal depth/size budgets. | V1.5 |
| MAIL-006 | Sender / recipient become graph entities with `PERSON` type. | V1.5 |
| MAIL-007 | Email body is `UNTRUSTED_EVIDENCE`, highest injection-risk class. | V1.5 |
| MAIL-008 | Never send, reply, or delete mail in V1/V2. Read-only. | — |
| MAIL-009 | Encrypted (S/MIME, PGP) messages: metadata only, flagged. | V2 |
| MAIL-010 | Calendar `.ics` ingestion feeds the timeline. | V2 |

## 45.4 Language and tokenization — `I18N`

| ID | Requirement |
|---|---|
| I18N-001 | Detect document language at parse time; store on `FileVersion`. |
| I18N-002 | Tantivy tokenizer selected per-field per-language. |
| I18N-003 | CJK support via dictionary tokenizer (Lindera or equivalent). |
| I18N-004 | Arabic / Hebrew RTL: correct text extraction and preview rendering. |
| I18N-005 | Mixed-language documents indexed under multiple tokenizers where detected. |
| I18N-006 | Embedding model choice must be multilingual or user warned of gaps. |
| I18N-007 | UI localizable; string externalization from day one. |
| I18N-008 | Date / number parsing locale-aware in deterministic extraction. |
| I18N-009 | Filename normalization: Unicode NFC/NFD (macOS uses NFD) must not create duplicate identities. |

## 45.5 Packaging, size, and update — `PKG`

| ID | Requirement | Target |
|---|---|---|
| PKG-001 | Base installer size budget. | ≤ 250 MB |
| PKG-002 | Total on-disk after default model download. | ≤ 900 MB |
| PKG-003 | Embedding models downloaded post-install, not bundled. | — |
| PKG-004 | Tree-sitter grammars: bundle top 10, lazy-fetch the rest. | — |
| PKG-005 | ONNX Runtime / PDFium linked as needed; strip unused EPs. | — |
| PKG-006 | Offline installer variant with models bundled (air-gapped SKU). | — |
| PKG-007 | Code signing + notarization (macOS), Authenticode (Windows). | — |
| PKG-008 | Delta updates for app binary. | — |
| PKG-009 | Update must not run while an action transaction is open. | — |
| PKG-010 | Rollback to previous app version must not corrupt DB (schema back-compat window: 1 minor version). | — |
| PKG-011 | Model files content-addressed and integrity-verified on download. | — |
| PKG-012 | Uninstall offers: keep index / delete index / delete everything. | — |

### Size budget breakdown (estimate)

| Component | Approx |
|---|---|
| Tauri shell + UI bundle | 15–25 MB |
| Rust daemon binary | 40–70 MB |
| Tantivy + SQLite | included above |
| PDFium | 8–12 MB |
| Tree-sitter (10 grammars) | 10–20 MB |
| ONNX Runtime (CPU + one accel EP) | 40–120 MB |
| Embedding model (small, quantized) | 30–120 MB |
| Reranker (optional) | 20–90 MB |
| **Total range** | **165–460 MB** |

## 45.6 Model and data licensing — `LIC`

| ID | Requirement |
|---|---|
| LIC-001 | Every redistributed model must have a commercial-use-permitted licence. |
| LIC-002 | Model licence, version, and SHA recorded in the model registry. |
| LIC-003 | Non-commercial models may be user-installed but never bundled. |
| LIC-004 | UI shows licence per installed model. |
| LIC-005 | Third-party crate licence audit in CI (deny GPL/AGPL in shipped binary unless cleared). |
| LIC-006 | Tree-sitter grammar licences audited individually. |
| LIC-007 | Cloud provider ToS around data retention surfaced in the privacy UI. |

## 45.7 Evaluation under privacy constraints — `EVAL`

**Reason:** Part 1 §29 defines metrics but no way to measure them without seeing user data.

| ID | Requirement |
|---|---|
| EVAL-001 | Maintain an internal permissioned corpus (staff-donated + synthetic + public). |
| EVAL-002 | Corpus must include every format, every failure mode from §28.3. |
| EVAL-003 | Golden query set with human-labelled relevance, versioned in-repo. |
| EVAL-004 | Every retrieval change runs the golden set in CI; regressions block merge. |
| EVAL-005 | Entity-merge false-positive rate measured on labelled subset before each release. |
| EVAL-006 | Opt-in telemetry captures **user correction events only** — never content. |
| EVAL-007 | Correction rate per extraction type is the production quality proxy metric. |
| EVAL-008 | Local self-eval mode: user can run a quality report on their own corpus, results never leave device. |
| EVAL-009 | Adversarial injection corpus run on every release (§29.4). |
| EVAL-010 | Published quality bar per phase — see §52. |

## 45.8 Sync and portability — `SYNC`

| ID | Requirement | Phase |
|---|---|---|
| SYNC-001 | V1/V2: single-device. Explicit non-goal, stated in UI. | V1 |
| SYNC-002 | Export: full knowledge bundle (SQLite + corrections + policies), documented format. | V1 |
| SYNC-003 | Import on a new machine rebuilds derived indexes from canonical state. | V1 |
| SYNC-004 | Corrections and policies portable independently of the index. | V1 |
| SYNC-005 | Entity IDs must be globally unique (ULID) to allow future merge. | V1 |
| SYNC-006 | Schema must carry `origin_device_id` on writes from day one. | V1 |
| SYNC-007 | Real multi-device sync deferred; schema must not preclude it. | V3 |

## 45.9 Telemetry — `TEL`

| ID | Requirement |
|---|---|
| TEL-001 | Off by default. Explicit opt-in during onboarding. |
| TEL-002 | Never transmit file names, paths, content, queries, or entity labels. |
| TEL-003 | Permitted: crash traces (symbolicated, sanitized), counters, latency histograms, feature usage, correction counts. |
| TEL-004 | User can view exactly what would be sent, before sending. |
| TEL-005 | Enterprise policy can disable telemetry unconditionally. |
| TEL-006 | Local diagnostics always available regardless of telemetry setting. |

## 45.10 Accessibility — `A11Y`

| ID | Requirement |
|---|---|
| A11Y-001 | Full keyboard navigation for search, results, approval dialogs. |
| A11Y-002 | Approval dialogs must be screen-reader complete — target paths read aloud. |
| A11Y-003 | Risk level conveyed by text + icon, never colour alone. |
| A11Y-004 | Respect OS reduced-motion and high-contrast settings. |
| A11Y-005 | Minimum contrast WCAG AA. |
| A11Y-006 | Graph explorer must have a non-visual list equivalent. |

## 45.11 Cost and budget governance — `BGT`

| ID | Requirement |
|---|---|
| BGT-001 | Per-workspace monthly token/cost ceiling, user-set. |
| BGT-002 | Per-run token/step/time/cost budget (already AGT-012) surfaced in UI. |
| BGT-003 | Cloud spend estimator shown before enabling cloud enrichment. |
| BGT-004 | Hard stop at ceiling; degrade to local, never silently overspend. |
| BGT-005 | Running total visible in Settings. |
| BGT-006 | Enterprise policy can set org-wide ceilings. |

---

# 46. Verification subsystem (new — was missing)

**Reason:** Part 1 mentions validators in passing (AGT-013, §12.4). Verification is a first-class subsystem and must be specified, because "read, modify **and verify**" is a core product promise.

## 46.1 Four verification layers

| Layer | Verifies | When | Blocking? |
|---|---|---|---|
| V1 — Input verification | Parsed content matches source | After parse | No (flags file) |
| V2 — Answer verification | Claims are supported by evidence | Before showing answer | Yes (rewrites/abstains) |
| V3 — Action verification | Result matches intent | After each mutation | Yes (triggers rollback) |
| V4 — Index verification | Derived state matches canonical | Background | No (schedules repair) |

## 46.2 V1 — Input verification

| Check | Method | On failure |
|---|---|---|
| Parse completeness | Extracted char count vs expected for size/type | Flag `LOW_YIELD`, suggest OCR |
| Structure sanity | Heading/table counts within plausible range | Warn, keep file discoverable |
| Encoding | Valid UTF-8 after conversion; replacement-char ratio | Re-detect encoding, retry once |
| Truncation | Parser reported EOF vs declared length | Mark `PARTIAL` |
| Archive safety | Count/size/depth/path budgets (PAR-010) | Abort extraction |
| Scanned-PDF detection | Text yield ≈ 0 with page count > 0 | Offer OCR, do not silently drop |

## 46.3 V2 — Answer verification (grounding)

| Step | Rule |
|---|---|
| 1. Claim extraction | Decompose answer into atomic claims |
| 2. Evidence binding | Each claim maps to ≥1 evidence ID |
| 3. Unsupported claim | Remove, or downgrade to hedged wording |
| 4. Numeric/date check | Verbatim match against source span, else flag |
| 5. Conflict check | If evidence contains contradictions, answer must state both |
| 6. Staleness check | If evidence file changed after retrieval, re-retrieve |
| 7. Abstention | If coverage < threshold, say "not found in your files" |
| 8. Citation render | Every claim clickable to exact page/line/cell |

| Metric | V1 bar | V2 bar |
|---|---|---|
| Claims with evidence binding | ≥ 85% | ≥ 95% |
| Hallucinated citation rate | < 2% | < 0.5% |
| Correct abstention on out-of-corpus questions | ≥ 80% | ≥ 92% |

## 46.4 V3 — Action verification (post-mutation)

| Target type | Validator | Rollback trigger |
|---|---|---|
| Source code | Tree-sitter reparse; optional `lint` / `build` / test subset | Parse error or new test failure |
| JSON / YAML / TOML | Schema-aware reparse + structural diff | Parse error or unintended key removed |
| DOCX | Reopen via document engine; verify structure intact | Open failure or lost sections |
| XLSX | Reopen; recalc affected range; verify no `#REF!` introduced | New formula error |
| Plain text / Markdown | Diff matches `PreparedAction` diff byte-for-byte | Mismatch |
| File create | Exists, size > 0, correct MIME | Any failure |
| Delete / move | Target state matches expectation; snapshot retained | Any mismatch |
| Shell | Exit code + stdout pattern + declared expected effect | Non-zero or unexpected side effect |
| Git | `git status` matches expected; no unintended staged files | Mismatch |

**Rule:** Every mutation tool must declare a validator. A tool with no validator is `Irreversible + Unverifiable` and requires elevated approval.

## 46.5 V4 — Index verification (self-healing)

| Check | Frequency | Repair |
|---|---|---|
| Canonical file count vs FTS doc count | Daily | Reindex delta |
| Canonical chunk count vs vector count | Daily | Re-embed delta |
| Orphan vectors (no live chunk) | Daily | Delete |
| Orphan facts (no live evidence) | Weekly | Tombstone per retention |
| Filesystem reconciliation drift | Per §45.2 interval | Rescan root |
| SQLite `PRAGMA integrity_check` | Weekly + on unclean shutdown | Restore from backup |
| Tantivy segment health | Weekly | Rebuild from canonical |
| Vector store checksum | Weekly | Rebuild from embedding cache |

---

# 47. Reversibility model (replaces uniform `rollback()`)

**Problem in Part 1:** `Tool::rollback()` on every tool implies everything is reversible. It is not.

## 47.1 Capability classes

| Class | Meaning | Example | Approval UX |
|---|---|---|---|
| `Reversible` | Exact prior state restorable | file patch, file create, move | "Undo available" |
| `Compensatable` | Inverse action exists, not identical | `git revert`, delete new remote branch | "Can be undone by a follow-up action" |
| `Irreversible` | No undo possible | email send, `git push`, HTTP POST, `rm` outside snapshot | **"This cannot be undone"** — red, explicit |
| `Unknown` | Tool has not declared | any shell command | Treated as `Irreversible` |

## 47.2 Revised trait

```rust
trait Tool {
    fn descriptor(&self) -> ToolDescriptor;
    fn risk(&self, args: &Value) -> RiskAssessment;
    fn reversibility(&self, args: &Value) -> Reversibility; // NEW - declared per-invocation
    fn validator(&self) -> Option<ValidatorId>;             // NEW - mandatory for mutations
    async fn prepare(&self, ctx: &ToolContext, args: Value) -> Result<PreparedAction>;
    async fn execute(&self, action: PreparedAction) -> Result<ToolResult>;
    async fn verify(&self, receipt: &ExecutionReceipt) -> Result<VerifyResult>; // NEW
    async fn compensate(&self, receipt: &ExecutionReceipt) -> Result<CompensateResult>;
}
```

## 47.3 Plan-level rules

| Rule |
|---|
| A plan's reversibility = weakest step in the plan. |
| Irreversible steps must be ordered **last** where the plan allows reordering. |
| Any irreversible step forces plan-level explicit approval, regardless of risk class. |
| Mixed plans show a per-step reversibility column in the approval UI. |
| Partial failure after an irreversible step → stop, report exact state, never auto-continue. |

## 47.4 Snapshot mechanics

| Item | Rule |
|---|---|
| Where | `<AppData>/lkar/transactions/<txn_id>/` |
| What | Pre-image of every file to be modified or deleted |
| Size cap | Per-transaction cap; exceed → warn and require explicit approval |
| Retention | Configurable, default 7 days or until disk pressure |
| Integrity | BLAKE3 of pre-image stored in the transaction record |
| Cleanup | On commit + retention expiry, or on explicit user purge |

---

# 48. Tier C budget governor (resolves §11.1 vs §28.1 contradiction)

**Problem:** 100k-file target + corpus-wide LLM enrichment = weeks of compute. Unshippable.

## 48.1 Rule

> **Tier C enrichment is demand-driven, never corpus-wide.** This is an architectural invariant, not a settings toggle.

## 48.2 Promotion triggers

| Trigger | Priority | Budget class |
|---|---|---|
| File appears in an answered query's evidence set | P1 | Immediate |
| File opened by the user in the last 7 days | P1 | Immediate |
| File modified in the last 24 h in an active workspace | P2 | Batched |
| File is a hub in the deterministic structural graph (high degree) | P3 | Idle only |
| User explicitly requests "understand this folder deeply" | P1 | Immediate, budgeted |
| Entity involved in a user correction | P2 | Batched |
| Nothing above | **Never** | Tier A + B only |

## 48.3 Expected coverage

| Corpus | Tier A+B | Tier C (realistic) |
|---|---|---|
| 100k files | 100% | 2–8% |
| 10k files | 100% | 10–25% |
| Single project folder | 100% | 40–90% |

## 48.4 Budget enforcement

| Control | Default |
|---|---|
| Max Tier C tokens per day (local) | Time-based: 60 min idle compute |
| Max Tier C tokens per day (cloud) | User cost ceiling (BGT-001) |
| Runs only when | Idle ≥ 3 min AND (AC power OR user override) |
| Preemption | Any interactive query preempts immediately |
| Backlog surfaced | "Deep understanding: 3,204 files pending" in index health card |

---

# 49. Local IPC authentication (was unspecified)

| Platform | Transport | Authentication |
|---|---|---|
| macOS / Linux | Unix domain socket in user-private dir, mode `0700` | `SO_PEERCRED` (Linux) / `LOCAL_PEERCRED` (macOS): assert peer UID == daemon UID |
| Windows | Named pipe, per-user name including SID | DACL restricted to the user SID; `GetNamedPipeClientProcessId` + token check |
| All | — | Reject if peer UID/SID mismatch. Log security event. |

| Rule |
|---|
| No localhost TCP for privileged operations, ever. |
| Client-supplied identity fields (`is_admin`, `user_id`) are ignored; authorization derives from peer credentials + daemon state. |
| Socket/pipe recreated with correct permissions on every daemon start. |
| Version handshake: daemon rejects clients with incompatible protocol version and reports upgrade needed. |
| Every IPC request carries `request_id` for tracing and cancellation. |

---

# 50. SQLite write architecture (was unspecified)

**Problem:** WAL allows one writer. Scanner, parsers, embedders, graph updaters all write.

| Decision | Detail |
|---|---|
| Topology | Single writer actor (one thread, `mpsc` inbox). All writes routed through it. |
| Readers | Unlimited, WAL read connections per worker. |
| Batching | Writer commits in batches: max 500 rows or 100 ms, whichever first. |
| Backpressure | Bounded inbox; producers block when full. Prevents unbounded memory during initial scan. |
| Pragmas | `journal_mode=WAL`, `synchronous=NORMAL`, `busy_timeout=5000`, `foreign_keys=ON`, `wal_autocheckpoint` tuned. |
| Checkpointing | Manual `TRUNCATE` checkpoint on idle to bound WAL growth. |
| Large blobs | Chunk text stored in DB only if < 64 KB; otherwise content-addressed file in `extraction-cache/`. |
| Backup | Pre-migration backup via `VACUUM INTO`. |
| Corruption | `integrity_check` on unclean shutdown; restore from backup; derived indexes rebuilt. |

**Expected throughput:** 5–20k row-writes/sec batched, sufficient for a 100k-file initial scan (target: scan+metadata < 20 min on SSD).

---

# 51. Sandbox posture — honest V1 vs V2

**Problem:** Part 1 §6.4 specifies three OS sandboxes. That is 2–4 months of specialist work and will slip.

| Platform | V1 posture (ship this) | V2 posture (target) |
|---|---|---|
| macOS | Separate parser process, non-sandboxed, restricted env; hardened runtime; security-scoped bookmarks for folder grants | Full App Sandbox for parser/exec workers |
| Windows | Separate parser process with restricted token (drop privileges, low integrity level) | AppContainer / Win32 App Isolation |
| Linux | Separate process + `seccomp` syscall filter + `Landlock` where kernel ≥ 6.7 | Landlock mandatory; bubblewrap fallback |
| All | Resource limits (CPU, memory, wall time, output size) on every worker | + network namespace denial |

| Rule |
|---|
| Ship V1 with the posture above and **state it accurately** in the security documentation. Do not claim sandboxing you do not have. |
| Shell execution is **disabled in V1**. It requires V2 sandbox posture. |
| Third-party parser plugins: **not supported in V1**. Requires V2 posture. |
| Landlock unavailability (old kernel) must be surfaced, not silent. |

---

# 52. Corrections to Part 1 (MCP 2026-07-28)

| Part 1 item | Correction |
|---|---|
| §15 — MCP features | Roots, Sampling, Logging are **deprecated** (SEP-2577). ~12-month support window. Do not build on them. |
| §15 — long-running work | Tasks moved to the `io.modelcontextprotocol/tasks` extension with poll-based `tasks/get` and `tasks/update`. Use it for agent runs. |
| §15 — transport | Legacy HTTP+SSE transport deprecated with a year-long offramp. Use Streamable HTTP, stateless mode. |
| §15 — capability discovery | `initialize` handshake removed. Use `server/discover`; protocol version and client capabilities travel in `_meta` per request. |
| §15 — server state | Cross-call state uses **server-minted handles passed as ordinary tool arguments** (SEP-2567). Matches ADR-009. |
| §15 — auth | Dynamic Client Registration deprecated in favour of CIMD. |
| §15 — notifications | Change notifications via `subscriptions/listen`, opt-in per notification type. |
| §17 — Rust SDK | Rust SDK support for 2026-07-28 was in beta at spec release. Verify GA status before Phase 6. |
| MCP-001 | Amend: "Support MCP 2026-07-28, stateless mode only, no deprecated features." |
| MCP-011 (new) | LKAR agent runs exposed via the Tasks extension, not custom polling. |
| MCP-012 (new) | Deprecated-feature usage blocked in CI lint. |

---

# 53. Revised delivery plan

## 53.1 The cut

| Phase | Content | Duration | Team |
|---|---|---|---|
| **P0** Spike | Benchmarks, threat PoC, stack decisions | 6–8 wk | 3 |
| **P1** Search | Folders, scan, watch, reconcile, SQLite, parsers (text/md/code/PDF-text), Tantivy, preview UI, index health | 5–7 mo | 3–4 |
| **P2** Semantic | Chunking, local embeddings, vector store, hybrid fusion, citations, model gateway, privacy UX, **V2 answer verification** | 3–4 mo | 4 |
| **P3** Safe actions | Policy engine, typed tools, prepare/approve/execute, **V3 action verification**, reversibility model, diffs, undo, structural edits (code/JSON/YAML/MD) | 5–7 mo | 4–5 |
| **P4** Graph | Entities, mentions, facts, deterministic structural graph, demand-driven Tier C, reversible resolution, correction UX, graph retrieval | 6–9 mo | 4–5 |
| **P5** Temporal & global | Timeline, Git integration, summary hierarchy, periodic community clustering, temporal planner | 4–6 mo | 3–4 |
| **P6** Rich formats & scale | DOCX/XLSX deep parsing, email, OCR, second platform, V2 sandbox, shell | 5–8 mo | 4–5 |
| **P7** Ecosystem | MCP server, MCP client, enterprise policy, private inference, plugin trust | 4–6 mo | 3–4 |

**Total: 34–51 months sequential. 20–28 months with two parallel tracks after P2.**

## 53.2 Key change vs Part 1

| Part 1 order | Revised order | Why |
|---|---|---|
| Graph (P3) before Actions (P5) | **Actions (P3) before Graph (P4)** | Actions are what users pay for. Graph is highest-risk / least-proven. Schema fields ship in P1 regardless. |
| Rich document parsing in P1 | **Moved to P6** | XLSX formula graphs and DOCX comments are months of work with low early ROI. |
| Daemon split from day one | **In-process + parser subprocess in P1; daemon split in P3** | Daemon costs services, upgrades, version skew. Defer until actions need it. |
| Shell execution in P5 | **P6, after V2 sandbox** | Cannot ship safely before sandbox posture is real. |

## 53.3 V1 shipping definition (P0+P1+P2)

| Ships | Does not ship |
|---|---|
| Selected-folder scanning with consent UI | Whole-OS indexing |
| Text, Markdown, code, PDF text-layer | XLSX formulas, DOCX comments, OCR, email |
| Lexical + semantic hybrid search | Knowledge graph |
| Source-cited answers with verification | Agent writes of any kind |
| Local + cloud model gateway | MCP |
| Full canonical schema (graph fields present, unpopulated) | Enterprise policy |
| One platform | Shell, plugins, sync |

**Time to V1: 9–13 months, 3–4 engineers.**

---

# 54. Cost model

## 54.1 Engineering cost

| Scenario | Headcount | Duration | Loaded cost @ $180k/yr |
|---|---|---|---|
| V1 only | 3.5 avg | 11 mo | ~$580k |
| Through P4 (graph) | 4.5 avg | 26 mo | ~$1.75M |
| Full spec (P0–P7) | 4.5 avg | 40 mo | ~$2.7M |
| Full spec, 2 parallel tracks | 9 avg | 24 mo | ~$3.2M |

**Excludes:** design, PM, QA, security audit, legal, infra.

| Additional line | Estimate |
|---|---|
| External security audit (pre-action-release) | $60–120k |
| Design/UX (0.5 FTE throughout) | ~$110k/yr |
| Code signing, notarization, CI/CD | $10–20k/yr |
| Model licensing / eval corpus construction | $30–80k one-off |

## 54.2 Runtime cost (per user)

| Mode | Embedding | Generation | Notes |
|---|---|---|---|
| Local only | $0 | $0 | Compute is user's. Battery is the real cost. |
| Cloud embedding, 100k files | $8–40 one-off | — | Depends on model + chunk count |
| Cloud generation, moderate use | — | $3–15/mo | Retrieval keeps context small |
| Cloud Tier C enrichment, demand-driven | — | $2–20/mo | With §48 governor |
| Cloud Tier C, corpus-wide (**do not do this**) | — | $150–900 one-off | Why §48 exists |

## 54.3 Compute cost (local, on-device)

| Task | 100k files, consumer laptop |
|---|---|
| Scan + metadata | 10–25 min |
| Parse (text/code/PDF-text) | 2–8 h |
| Embedding (small model, CPU) | 8–30 h |
| Embedding (GPU/NPU accelerated) | 1.5–6 h |
| Tier C demand-driven (5% coverage) | 20–60 h spread over weeks |
| Tier C corpus-wide | 300–900 h ← **not viable** |

---

# 55. Risk register

| # | Risk | Prob | Impact | Mitigation | Owner |
|---|---|---|---|---|---|
| R1 | Document parser effort 2–3× estimate | High | High | Defer XLSX/DOCX depth to P6; PDF text-layer only in V1 | Eng lead |
| R2 | Knowledge graph quality below usable bar | High | High | Gate P4 exit on EVAL metrics; deterministic edges are the fallback product | AI lead |
| R3 | Cloud placeholder hydration incident | Medium | **Severe** (trust) | §45.1 mandatory in P1 | Eng lead |
| R4 | Incremental community detection not achievable | High | Medium | Periodic full re-clustering, bounded graph | AI lead |
| R5 | Sandbox work slips | High | Medium | §51 honest V1 posture; shell deferred | Security |
| R6 | Prompt injection reaches a mutation | Low | **Severe** | Policy engine + §29.4 corpus in CI + external audit | Security |
| R7 | Installer size exceeds tolerance | Medium | Medium | §45.5 budget; post-install model download | Eng lead |
| R8 | Local embedding too slow on low-end hardware | Medium | Medium | Ship lexical-first; embeddings opt-in with hardware check | AI lead |
| R9 | SQLite write contention at scale | Medium | Medium | §50 single-writer batching, load-tested in P0 | Eng lead |
| R10 | False entity merges poison graph | Medium | High | Reversible merges + conservative threshold + user correction loop | AI lead |
| R11 | MCP spec changes again before P7 | Medium | Low | Adapter isolation (ADR-009) already mitigates | Eng lead |
| R12 | Rust ecosystem gaps (Leiden, DOCX, OOXML) force in-house builds | High | Medium | Verify in P0; budget contingency | Eng lead |
| R13 | Agent modifies file mid-edit by user | Medium | High | §25 optimistic concurrency, already specced | Eng lead |
| R14 | Team cannot hire Rust + AI + security specialists | Medium | High | Reduce to V1 scope; contract security work | Leadership |

---

# 56. Phase exit criteria (gates)

| Phase | Must pass to exit |
|---|---|
| P0 | Vector store benchmark data; embedding throughput on 3 hardware tiers; path-escape PoC blocked; parser crash isolation demonstrated; SQLite write-rate load test |
| P1 | 100k-file scan < 30 min; lexical p95 < 100 ms; watcher reconciliation drift = 0 in 72 h soak; zero placeholder hydration; crash-recovery of job queue verified |
| P2 | Golden query set Recall@10 ≥ 0.80; hybrid p95 < 500 ms (reranker excluded); claim-evidence binding ≥ 85%; abstention ≥ 80%; cloud egress shows correct byte counts |
| P3 | 100% of mutations have declared reversibility + validator; adversarial injection corpus 0 escapes; undo success ≥ 99% on `Reversible` class; stale-write rejection 100% |
| P4 | Entity precision ≥ 0.85; merge false-positive ≤ 2%; 100% of facts have provenance; merge reversal correctness 100%; Tier C stays inside budget governor |
| P5 | Timeline correctness vs Git ground truth ≥ 98%; summary staleness propagation correct; global query beats naive RAG baseline on golden set |
| P6 | XLSX formula graph accuracy ≥ 95% on test workbooks; second platform at parity; V2 sandbox verified by external audit; shell escape tests pass |
| P7 | MCP conformance suite passes; external client sees only approved capabilities; policy denial rate correct on adversarial MCP server |

---

# 57. Requirement coverage index (Part 1 + Part 2)

| Block | Prefix | Count | Source |
|---|---|---|---|
| Workspace / consent | WS | 12 | Part 1 |
| Filesystem discovery | FS | 16 | Part 1 |
| Parsing | PAR | 14 | Part 1 |
| Chunking | CHK | 8 | Part 1 |
| Lexical index | IDX | 7 | Part 1 |
| Embeddings | EMB | 10 | Part 1 |
| Knowledge graph | KG | 14 | Part 1 |
| Temporal | TMP | 6 | Part 1 |
| Retrieval | RET | 12 | Part 1 |
| Model gateway | MOD | 9 | Part 1 |
| Agent / tools | AGT | 15 | Part 1 |
| MCP | MCP | 12 | Part 1 + §52 |
| Security | SEC | 18 | Part 1 |
| UX | UX | 16 | Part 1 |
| Reliability / perf | NFR | 12 | Part 1 |
| **Tiered storage** | **TIER** | **12** | **Part 2** |
| **Watcher limits** | **WATCH** | **10** | **Part 2** |
| **Email** | **MAIL** | **10** | **Part 2** |
| **I18N** | **I18N** | **9** | **Part 2** |
| **Packaging** | **PKG** | **12** | **Part 2** |
| **Licensing** | **LIC** | **7** | **Part 2** |
| **Evaluation** | **EVAL** | **10** | **Part 2** |
| **Sync / portability** | **SYNC** | **7** | **Part 2** |
| **Telemetry** | **TEL** | **6** | **Part 2** |
| **Accessibility** | **A11Y** | **6** | **Part 2** |
| **Budget** | **BGT** | **6** | **Part 2** |
| **Total** | | **~276** | |

---

# 58. Updated architecture checklist (supersedes Appendix A)

| # | Check | Phase |
|---|---|---|
| 1 | Path ≠ identity in schema | P1 |
| 2 | Parser / embedding / model versions stored | P1 |
| 3 | Indexes rebuildable from canonical state | P1 |
| 4 | Watcher + reconciliation + degraded-mode UI | P1 |
| 5 | **Placeholder files never hydrated** | P1 |
| 6 | **inotify watch-limit strategy implemented** | P1 |
| 7 | Job queue survives crash | P1 |
| 8 | **Single-writer SQLite actor with batching** | P1 |
| 9 | **Local IPC peer-credential auth** | P1/P3 |
| 10 | Semantic search optional; lexical standalone | P1 |
| 11 | Graph schema fields present (unpopulated OK) | P1 |
| 12 | **Export/import bundle works** | P1 |
| 13 | Generational vector indexes + dual read | P2 |
| 14 | **Answer verification: claim→evidence binding** | P2 |
| 15 | **Abstention behaviour correct** | P2 |
| 16 | Cloud egress classified, visible, byte-counted | P2 |
| 17 | OS keyring for all credentials | P2 |
| 18 | No file content can grant tool authority | P3 |
| 19 | Model cannot bypass policy engine | P3 |
| 20 | Symlink / path-escape tests pass | P3 |
| 21 | **Every mutation declares reversibility class** | P3 |
| 22 | **Every mutation has a validator** | P3 |
| 23 | **Irreversible actions labelled explicitly in UI** | P3 |
| 24 | Stale-version check before write | P3 |
| 25 | Transaction snapshot + undo works | P3 |
| 26 | Prompt-injection corpus passes | P3 |
| 27 | Provenance exists for every graph fact | P4 |
| 28 | Entity merge reversible | P4 |
| 29 | Contradiction states supported | P4 |
| 30 | **Tier C demand-driven only, budget-enforced** | P4 |
| 31 | Deletion removes derived knowledge correctly | P4 |
| 32 | Indexing resource governor works | P1→ |
| 33 | Diagnostics contain no file bodies | P1→ |
| 34 | **Telemetry opt-in, content-free, inspectable** | P2 |
| 35 | **Accessibility: approval dialogs screen-reader complete** | P3 |
| 36 | **Sandbox posture documented accurately** | P3 |
| 37 | External MCP goes through policy engine | P7 |
| 38 | **No deprecated MCP features used** | P7 |

---

# 59. Decision log — items still open

| # | Decision | Blocked by | Deadline |
|---|---|---|---|
| D1 | LanceDB vs Qdrant Edge | P0 benchmark | End P0 |
| D2 | ONNX Runtime vs Candle | P0 benchmark | End P0 |
| D3 | Tantivy vs FTS5-only compact profile | P0 benchmark | End P0 |
| D4 | PDF engine: PDFium vs pure-Rust | P0 parser eval | End P0 |
| D5 | First platform: macOS or Windows | Market data | Start P1 |
| D6 | Embedding model choice (multilingual?) | Licence + benchmark | End P0 |
| D7 | Daemon split timing | P3 needs assessment | Start P3 |
| D8 | Graph store migration threshold | Measured traversal load | End P4 |
| D9 | Community detection: incremental vs periodic | P5 spike | Start P5 |
| D10 | Business model / pricing | Not addressed anywhere yet | Before P2 |
| D11 | Competitive positioning | Not addressed anywhere yet | Before P1 |

---

# 60. What Part 1 + Part 2 still do not cover

| Gap | Why deferred | Needs |
|---|---|---|
| Business model, pricing, packaging tiers | Out of architectural scope | Product/commercial doc |
| Competitive analysis | Out of architectural scope | Market doc |
| Go-to-market, distribution | Out of scope | — |
| Support / escalation process | Post-launch | Ops doc |
| SOC2 / ISO / regulatory certification path | Enterprise SKU only | Compliance doc |
| Data processing agreements for cloud providers | Legal | Legal review |
| Multi-user / shared machine behaviour | Edge case | Add to P3 design |
| Mobile / web companion | Explicit non-goal | — |
