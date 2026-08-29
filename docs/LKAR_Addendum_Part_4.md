# LKAR — Master Specification, Part 4

## Multi-User Behaviour, Commercial Model, Compliance, and Operations

**Status:** ⚠️ **SUPERSEDED BY PART 7.** Retained, not deleted — the project is currently solo self-use and open source, so nothing here is in scope. Revisit only if that changes. See [Part 7 §124](LKAR_Addendum_Part_7.md) for the item-by-item disposition.
**Still relevant under Part 7:** §78 (reduced to MULTI-002/007/011), §82 category map (build reference only), §83.3 launch gates (repurposed as personal quality bars).
**Date:** 30 August 2026
**Numbering:** Continues from §76 of Part 3
**Format:** Tables and points only

---

# 77. Purpose and gap-closure map

Part 2 §60 listed eight items as explicitly uncovered. Part 3 closed none of them. This part closes all eight.

| # | Gap declared in §60 | Closed in | Status |
|---|---|---|---|
| 1 | Business model, pricing, packaging tiers | §79, §80, §81 | Closed |
| 2 | Competitive analysis | §82 | Closed — framework and category map; market data requires quarterly refresh |
| 3 | Go-to-market, distribution | §83 | Closed |
| 4 | Support / escalation process | §84 | Closed |
| 5 | SOC 2 / ISO / regulatory certification path | §86 | Closed — path and sequencing; requires counsel review before commitment |
| 6 | Data processing agreements for cloud providers | §87 | Closed |
| 7 | Multi-user / shared machine behaviour | §78 | Closed — new `MULTI` block, lands in P1 |
| 8 | Mobile / web companion | §79.4 | Restated as a non-goal, with rationale and the single deferred exception |

## 77.1 Evidence class of this document

Parts 1–3 are architecture, which is verifiable against code. Part 4 is substantially commercial and legal, which is not. Every table below is tagged.

| Tag | Meaning |
|---|---|
| **[DERIVED]** | Follows from the architecture in Parts 1–3. Same confidence as those parts. |
| **[ASSUMPTION]** | A defensible starting position that has **not** been validated against market or legal reality. Must be tested before commitment. |
| **[COUNSEL]** | Requires qualified legal review before it is acted on. Written as a checklist for counsel, not as legal advice. |

## 77.2 The one architectural item in this part

§78 (`MULTI`) is the only §60 gap that touches schema and process design. It must land in **P1**, because retrofitting per-user isolation after an index format ships is a migration, not a patch. Everything else in Part 4 can be built in parallel with, or after, engineering.

---

# 78. Multi-user and shared-machine behaviour — `MULTI` **[DERIVED]**

Part 2 §60 deferred this as an "edge case" to be added to P3 design. That assessment is wrong on timing: the isolation boundary is a P1 schema and IPC concern. The *UX* for it is P3.

## 78.1 Scenarios

| # | Scenario | Prevalence | Failure if unhandled |
|---|---|---|---|
| S1 | One OS account, one human | Dominant | — |
| S2 | Shared machine, separate OS accounts | Common (family, lab, small office) | Cross-account index and knowledge leakage |
| S3 | Shared machine, **one shared OS account** | Common (lab, kiosk, small business, shift work) | Total knowledge leakage between humans; LKAR cannot detect it |
| S4 | macOS fast user switching with daemon running | Common | Daemon serves the wrong session; background indexing continues for an inactive user |
| S5 | Windows multi-session / RDS / VDI | Enterprise | Named-pipe collision; multiple daemons contending for one data dir |
| S6 | Roaming profiles / folder redirection | Enterprise Windows | SQLite and Tantivy on SMB — corruption risk, unreliable watchers |
| S7 | Admin installs, standard user runs | Common enterprise | Data directories owned by the wrong principal; silent write failures |
| S8 | Same human, two machines | Covered by SYNC-001 (single-device, explicit non-goal) | — |

## 78.2 Requirements

| ID | Requirement | Phase |
|---|---|---|
| MULTI-001 | One daemon instance **per OS user session**. Never a machine-wide service, never a `LaunchDaemon` / system service. | P1 |
| MULTI-002 | All index, config, cache and transaction state lives under the **per-user** application data directory. Never `ProgramData`, never `/Library/Application Support` (machine scope). | P1 |
| MULTI-003 | UDS path / named-pipe name incorporates the user UID/SID **and** the session identifier (extends §49). | P1 |
| MULTI-004 | Peer-credential check (§49) is the authoritative boundary. A guessed socket path from another user must still be rejected. Path secrecy is not a control. | P1 |
| MULTI-005 | macOS fast user switching: daemon suspends indexing on session deactivate, resumes on activate. Interactive queries only from the active session. | P3 |
| MULTI-006 | Windows multi-session: pipe name includes the terminal session ID; one daemon per session; no shared mutex on the data dir. | P3 |
| MULTI-007 | Detect index storage on a redirected/network profile path. **SQLite and Tantivy over SMB/NFS are unsafe.** Warn and offer a local-disk index location. | P1 |
| MULTI-008 | Under a shared OS account (S3) LKAR **cannot** distinguish humans. Onboarding must state this plainly rather than imply protection that does not exist. | P1 |
| MULTI-009 | Optional **workspace lock**: passphrase-gated workspace with index encrypted at rest (SEC-006), key in the OS keychain. Partially mitigates S3. | P3 / Enterprise |
| MULTI-010 | Installation is machine-scoped; data is user-scoped. Uninstall by one user must never delete another user's index. | P1 |
| MULTI-011 | Elevation is never required at runtime. An app that needs admin to index is a defect, not a configuration. | P1 |
| MULTI-012 | Enterprise managed policy (§22.1) is machine-scoped, read-only to users, and applies to every user session on the device. | P3 |
| MULTI-013 | Removable and shared volumes: index rows are per-user. Identical content hashes across users are expected and are not a merge signal. | P3 |
| MULTI-014 | Diagnostic bundles (§23.3) must never contain another user's paths, workspace names or counts. | P3 |
| MULTI-015 | Models and Tree-sitter grammars may be a machine-shared **read-only, integrity-verified** cache (PKG-011). Indexes, graphs and corrections never are. | P1 |

## 78.3 Honest capability statement (must appear in onboarding and security docs)

| LKAR protects against | LKAR does **not** protect against |
|---|---|
| Another OS user reading your index through the app | Another human using **your** OS account |
| Another OS user connecting to your daemon | An OS administrator with full disk access |
| Accidental cross-account leakage in diagnostics | Local malware running as you |
| A second session writing to your database | Disk-level forensic recovery, unless workspace lock is enabled |

**Rule:** the security documentation states the right-hand column explicitly. This mirrors §51 — do not claim isolation you do not have.

## 78.4 Interaction with existing requirements

| Existing | Interaction |
|---|---|
| §49 IPC auth | MULTI-003/004 extend it from "peer UID" to "peer UID + session" |
| SEC-006 encryption at rest | Becomes user-visible as MULTI-009 workspace lock, not only an enterprise deployment mode |
| SYNC-005/006 (ULID, `origin_device_id`) | Extend to `origin_principal_id` so a future merge can distinguish device from account |
| §23.3 diagnostics | MULTI-014 adds a per-user scrub check to the bundle builder |
| WS-011 enterprise forced roots | MULTI-012 defines the scope at which that policy is read |

---

# 79. Product packaging and SKUs **[ASSUMPTION]**

## 79.1 SKU definitions

| SKU | Audience | Core promise | Gating principle |
|---|---|---|---|
| **Free** | Anyone; evaluation | Local search that works with no account, no network, no model | Everything in P1 |
| **Pro** | Individual knowledge worker, developer | Semantic search, cited answers, safe actions | P2 + P3 |
| **Team** | 2–50 seats, no IT department | Pro + shared workspace policy templates, seat management, priority support | Pro + light management |
| **Enterprise** | Managed fleet | Team + signed managed policy, audit export, provider allowlists, private inference, MCP allowlist, SSO for the management console | P7 features |
| **Regulated / Air-gapped** | Defence, health, finance, classified | Enterprise + offline installer with bundled models, telemetry hard-disabled, offline activation | Enterprise + PKG-006 |

## 79.2 Feature-to-SKU matrix

| Capability | Free | Pro | Team | Ent | Air-gap | Phase |
|---|---|---|---|---|---|---|
| Folder consent, scan, watch, reconcile | ✅ | ✅ | ✅ | ✅ | ✅ | P1 |
| Lexical + metadata search, source preview | ✅ | ✅ | ✅ | ✅ | ✅ | P1 |
| Embedded media metadata (`META`) | ✅ | ✅ | ✅ | ✅ | ✅ | P1 |
| Export / import knowledge bundle (SYNC-002) | ✅ | ✅ | ✅ | ✅ | ✅ | P1 |
| Workspace count | 1 | Unlimited | Unlimited | Unlimited | Unlimited | P1 |
| Semantic search + embeddings | — | ✅ | ✅ | ✅ | ✅ | P2 |
| Cited answers + answer verification | — | ✅ | ✅ | ✅ | ✅ | P2 |
| Cloud model gateway (BYO key) | — | ✅ | ✅ | ✅ | — | P2 |
| Local model gateway | ✅ | ✅ | ✅ | ✅ | ✅ | P2 |
| Agent actions, diffs, undo | — | ✅ | ✅ | ✅ | ✅ | P3 |
| Knowledge graph + corrections | — | ✅ | ✅ | ✅ | ✅ | P4 |
| Timeline, Git integration, global queries | — | ✅ | ✅ | ✅ | ✅ | P5 |
| OCR, image/video/audio understanding | — | ✅ | ✅ | ✅ | ✅ | P5–P6 |
| Workspace lock / index encryption (MULTI-009) | — | — | ✅ | ✅ | ✅ | P3 |
| Shared policy templates, seat management | — | — | ✅ | ✅ | ✅ | P7 |
| Signed managed policy (§22.1) | — | — | — | ✅ | ✅ | P7 |
| Audit export, DLP hooks, provider allowlist | — | — | — | ✅ | ✅ | P7 |
| Private inference gateway (§22.2) | — | — | — | ✅ | ✅ | P7 |
| MCP server + client | — | ✅ | ✅ | ✅ | ✅ | P7 |
| Offline installer with bundled models | — | — | — | — | ✅ | P7 |
| Telemetry | Opt-in | Opt-in | Opt-in | Policy | Hard off | P2 |

## 79.3 What is never gated

**Rule: safety, privacy and data ownership are never paid features.** Gating them creates an incentive to ship an unsafe free tier, and it is indefensible publicly.

| Never gated | Why |
|---|---|
| Untrusted-content boundary, prompt-injection defence (SEC-003/004) | A free user's machine is not a cheaper target |
| Path canonicalization and symlink escape defence (SEC-001/002) | Same |
| Approval UX and risk labelling for any mutation that exists in the tier | An unapproved write is a defect at every price |
| Reversibility labelling (§47) | Same |
| Local-only / no-network mode (SEC-015) | The core promise of the product |
| OS keyring credential storage (SEC-005) | Same |
| Export of the user's own knowledge bundle (SYNC-002) | Data ownership is not a feature; hostage-taking is a churn mechanism |
| "Forget" semantics (§26) | Deletion is a right, not an upsell |
| Accessibility (`A11Y`) | Legal exposure and basic decency |

## 79.4 Mobile / web companion — restated non-goal

| Question | Answer |
|---|---|
| Is it planned? | No. Explicit non-goal through V3. |
| Why? | The index, the policy engine and the evidence live on one device by architectural choice (SYNC-001). A mobile client implies either syncing the corpus (contradicts local-first) or a hosted index (contradicts the entire product thesis). |
| Is there a defensible version? | One: **read-only remote query against the user's own running desktop**, over a user-controlled relay, with no index leaving the device and answers rendered without file bodies. |
| When? | V3 at the earliest, and only after P7. It is a distribution feature, not a product pivot. |
| What it must never become | A hosted index. If the corpus lands on our servers, LKAR is a different product with a different threat model, different compliance surface (§86) and no moat (§82.2). |

---

# 80. Business model and pricing **[ASSUMPTION]**

## 80.1 Models considered

| Model | Fit with LKAR | Verdict |
|---|---|---|
| Perpetual licence + paid major upgrades | Matches local-first; poor for funding 40-month roadmap | Offer as an option, not the default |
| Per-seat subscription | Standard, predictable, funds the roadmap | ✅ Core |
| Freemium with a genuinely useful free tier | P1 is a complete, shippable product on its own — rare and valuable | ✅ Core |
| Usage-based on tokens | Punishes local inference; makes the privacy promise commercially inconvenient; unpredictable for users | ✗ Reject as primary |
| Open-core | Attractive for trust; risks giving away the policy engine, the actual moat | Defer; revisit at P7 |
| Ad / data monetisation | Directly contradicts the product | ✗ Never |

## 80.2 Recommended model

> **Freemium + per-seat subscription, with bring-your-own inference by default.**

| Principle | Consequence |
|---|---|
| **LKAR does not resell inference in Free/Pro/Team.** User supplies a provider key, or runs local models. | COGS per user ≈ $0. Gross margin ≈ software margin. The privacy story stays clean: we never hold the provider relationship over user content. |
| Free tier is P1 in full, not a crippled demo. | P1 is genuinely useful (search that beats Spotlight on structure). It is also the cheapest possible distribution of the index format. |
| Paid tiers begin at semantic + answers (P2) and actions (P3). | These are the capabilities users can feel the value of within one session. |
| Managed inference is an **Enterprise add-on only**. | Where a compliance department wants a single contracted processor, not 400 individual API keys. This is the only place we take inference COGS, and it is priced with margin. |

## 80.3 Indicative price points — **[ASSUMPTION — must be validated before P2]**

| SKU | Monthly | Annual | Notes |
|---|---|---|---|
| Free | $0 | $0 | No account required. No telemetry required. |
| Pro | $10–15 | $96–144 | Individual. BYO key. |
| Team | $18–25 / seat | $180–240 / seat | Min 2 seats. Adds policy templates + workspace lock. |
| Enterprise | From $30 / seat | Annual contract | Min commitment. Managed policy, audit, private inference. |
| Enterprise + managed inference | +$15–40 / seat | Annual | Priced above pass-through cost. Optional. |
| Regulated / Air-gapped | Custom | Annual | Offline activation, bundled models, support SLA. |
| Perpetual (Pro) | — | $220–300 one-off | Includes 12 months of updates; then feature-frozen but fully functional. |

**Validation required before committing:** willingness-to-pay research on the developer/prosumer beachhead (§83.1), price sensitivity of the perpetual option, and whether Team's differentiation is strong enough to exist at all — it may collapse into Pro.

## 80.4 Unit economics **[ASSUMPTION]**

| Line | Per paid user / year | Note |
|---|---|---|
| Inference COGS | **$0** | BYO key. This is the structural advantage. |
| Update / model CDN egress | $0.30–2.00 | Delta updates (PKG-008); model downloads are the bulk |
| Licence + activation service | $0.05–0.20 | Low-traffic, cacheable |
| Crash + opt-in telemetry ingest | $0.10–0.60 | Only for opted-in users |
| Support (blended, incl. free-tier deflection) | $6–22 | Dominant variable cost; see §84 |
| Payment processing | ~3% | |
| **Gross margin at Pro** | **~80–88%** | Support cost is the swing factor, not infrastructure |

**The lesson in this table:** for a local-first desktop product, support is the cost centre, not compute. §84 is therefore a margin document as much as an operations one.

## 80.5 Why not usage-based pricing

| Reason |
|---|
| It prices the thing we tell users to avoid doing (cloud enrichment, §48). |
| Local inference generates no billable event, so the incentive inverts against the product's own architecture. |
| It makes the cost of an agent run unpredictable at exactly the moment the user is being asked to approve a risky action (§16.7). Approval anxiety and billing anxiety must not be combined. |
| BGT-001..006 already give the user cost ceilings for *their own* provider spend. That is the right place for usage accounting. |

## 80.6 Trial and conversion

| Item | Position |
|---|---|
| Trial | 14 days of Pro, no card, triggered by first use of a paid capability rather than at install |
| Activation metric | **First cited answer opened to source.** Not install, not index-complete. |
| Expiry behaviour | Reverts to Free. Index retained in full. Never deleted, never locked. See ENT-006. |
| Upgrade prompt | At the point of capability, never as a modal on launch |
| Anti-pattern | Blocking search behind a trial expiry. Search is the free tier; taking it back destroys the trust the free tier bought. |

## 80.7 Pricing risks

| # | Risk | Mitigation |
|---|---|---|
| PR1 | Free tier (P1) is good enough that few convert | Instrument the P2 activation metric during beta; if conversion is weak, the boundary moves to actions (P3), not to crippling search |
| PR2 | BYO-key friction blocks non-technical Pro users | Ship first-class local models so a key is never required; make key entry a 30-second flow with a validation test |
| PR3 | Enterprise expects the SaaS features a local product cannot offer (central index, cross-user search) | Qualify out early; §82.3 names this loss honestly |
| PR4 | Perpetual licence cannibalises subscription | Keep it above 20 months of subscription value; exclude it from Team/Enterprise |
| PR5 | Price anchored against free OS assistants | Position on provenance and action safety (§82.2), never on "AI features" |

---

# 81. Entitlement and licence enforcement — `ENT` **[DERIVED + ASSUMPTION]**

The hard constraint: **enforcement must not break local-first, offline or air-gapped operation.** A product that stops working on a plane has broken its core promise to sell a subscription.

| ID | Requirement |
|---|---|
| ENT-001 | Licence is a **signed offline token** (Ed25519), verified against a public key compiled into the binary. No network call is required to validate. |
| ENT-002 | Token carries: SKU, seat count, issue date, expiry, grace window, feature flags, issuing key ID. It carries **no** user content identifiers. |
| ENT-003 | Online refresh is opportunistic. Failure to reach the licence service **never** degrades the app inside the grace window. |
| ENT-004 | Grace window default: 30 days past expiry for consumer, 90 days for Enterprise. Surfaced in the UI well before it lapses. |
| ENT-005 | Air-gapped activation: an offline request file is exported, signed out-of-band, and imported. No network at any point. |
| ENT-006 | **On expiry the app reverts to Free. It never deletes an index, never locks a workspace, never withholds export (SYNC-002).** The user's knowledge is the user's. |
| ENT-007 | No hardware fingerprinting that breaks on OS upgrade, disk replacement or motherboard change. Use a rotating install ID plus server-side seat reconciliation. |
| ENT-008 | Seat overuse is handled by notification and reconciliation, not by locking the newest device out mid-session. |
| ENT-009 | Enterprise uses a site licence file distributed with managed policy (§22.1). No per-device call-home is required or permitted. |
| ENT-010 | Licence state is stored in the OS credential store, not in `settings.json` (SEC-005 applies to entitlement too). |
| ENT-011 | Entitlement checks live at one call site per gated capability, are auditable, and are covered by tests that assert the *ungated* set (§79.3) is unreachable from entitlement code. |
| ENT-012 | Piracy posture: accept leakage. Every anti-piracy mechanism that inconveniences honest users costs more in support (§80.4) than the revenue it recovers. |

---

# 82. Competitive analysis and positioning **[ASSUMPTION]**

> **Caveat:** the product landscape as of August 2026 moves faster than this document can. What follows is a durable **category map and differentiation framework**. Specific vendor capabilities and prices must be re-verified before any external use — see §82.6.

## 82.1 Category map

| # | Category | Exemplars | What they do well | Structural gap LKAR fills |
|---|---|---|---|---|
| C1 | OS search & launchers | Spotlight, Windows Search, Everything, Alfred, Raycast | Instant filename/metadata search; zero install cost | No semantics, no provenance, no cross-document reasoning, no actions |
| C2 | Note tools with local AI | Obsidian + plugins, Logseq, Reor | Excellent inside their own vault; strong communities | Scoped to their own note format; the user's PDFs, spreadsheets, code and mail are outside |
| C3 | Local RAG desktop apps | LM Studio, GPT4All, AnythingLLM, Jan | Easy local model hosting; private by construction | Manual collections; no continuous ingestion, no incremental reindex, no provenance discipline, no action safety model |
| C4 | OS-vendor assistants | Windows Copilot / Recall, Apple Intelligence | Deep OS integration, zero install, free | Closed model, vendor-locked; capture-based rather than structure-based; no evidence graph the user can inspect, correct or export |
| C5 | Enterprise search / RAG platforms | Glean, Hebbia, Onyx, Elastic-based stacks | Cross-SaaS coverage, admin controls, scale | Server-side index; content leaves the device; wrong shape for an individual's local corpus |
| C6 | Coding agents | Claude Code, Cursor, Windsurf | Best-in-class repo understanding and code editing | Repo-scoped; not a cross-format personal knowledge system; not designed for PDFs, spreadsheets, mail or long-lived provenance |
| C7 | Continuous-capture memory | Rewind, Limitless-class | Recall of everything seen | Screen capture is a different consent and threat model; weak structured provenance; strong privacy resistance |

## 82.2 Defensible differentiators, ranked

| Rank | Differentiator | Why it is defensible |
|---|---|---|
| 1 | **Provenance to page / cell / line / timestamp** | It is an architectural property of the parser tier model (§63) and the IR (§8.6). It cannot be bolted onto a flat-Markdown pipeline later — that is precisely the §61 argument. |
| 2 | **Deterministic-first knowledge with authority classes** | §11.2 / §70.1. Competitors that start from LLM extraction cannot retrofit an authority ordering without re-extracting their whole corpus. |
| 3 | **Model replaceability** | The knowledge survives model churn (§1). Every vendor-assistant competitor is structurally the opposite. |
| 4 | **Action safety: policy below the model** | §6, §14, §47. This is the hardest thing in the spec to copy quickly, and the thing enterprises will diligence hardest. |
| 5 | **Local-first with honest egress accounting** | §32, UX-013. "Honest" is the differentiator, not "local" — several competitors are local. |
| 6 | **Continuous ingestion without manual collections** | §10. Removes the setup tax that kills C3 adoption. |
| 7 | **Reversible user corrections that change future answers** | §11.4, KG-005/007. Converts users into the quality mechanism (EVAL-007). |

## 82.3 Where LKAR loses — state this internally and do not paper over it

| Loss | Against | Reality |
|---|---|---|
| Raw filename search speed on day one | Everything, Spotlight | They have a head start measured in years of OS integration and no cold-index period |
| Zero install, zero disk, zero battery | C4 OS assistants | We cost 400 MB and hours of first index (§72) |
| Cross-SaaS live coverage (Slack, Jira, Gmail live) | C5 Glean-class | We are file-and-device shaped. Mail is read-only and file-based (`MAIL`), and only at V1.5+ |
| Code editing ergonomics | C6 Cursor / Claude Code | Their loop is tighter and their surface is the editor |
| Instant answers on a cold corpus | Everyone with a server-side index | Local first-index time is a real cost (§54.3) |
| Distribution | C4 | Pre-installed beats downloaded |

## 82.4 Positioning

| Element | Statement |
|---|---|
| **For** | Knowledge workers and developers whose real corpus is on their own disk |
| **Who** | Need to find, understand and act on it without uploading it |
| **LKAR is** | A local knowledge runtime with a replaceable model |
| **That** | Answers with citations to the exact page, cell or line — and takes actions it can show you, verify and undo |
| **Unlike** | Cloud assistants that need your files, and local chat apps that need you to build collections by hand |
| **Because** | The durable asset is your provenance-backed knowledge, not the model of the month |
| **Anti-positioning** | Not "ChatGPT for your files." Not a screen recorder. Not an enterprise search platform. Not a coding agent. |

## 82.5 Moat durability — what happens when an OS vendor ships this

| Their advantage | Our answer |
|---|---|
| Pre-installed, zero friction | We win on inspectable provenance and export. Users who care about being able to *check* the answer are our market. |
| Free | Free tier is P1 in full (§79.2). We compete on the paid capabilities they cannot ship: model choice, action safety, corrections. |
| OS-level file access | Ours is consent-scoped and visible (WS-006). For regulated and privacy-sensitive users, that is a feature, not a limitation. |
| Their model is their business | Ours is replaceable, including with theirs. Model neutrality is a position they cannot take. |
| **Genuine risk** | If an OS vendor ships provenance-grade citation and an inspectable evidence graph, differentiator #1 and #2 erode. Monitor this specifically (§82.6). It is the single highest-impact competitive event. |

## 82.6 Competitive monitoring

| Item | Cadence | Owner | Trigger for strategy review |
|---|---|---|---|
| OS-vendor assistant capabilities | Monthly | Product | Any shipped citation-to-source-location feature |
| C3 local RAG apps: ingestion + provenance | Quarterly | Product | Continuous incremental ingestion appearing in a free tool |
| C5 enterprise search moving on-device | Quarterly | Product | An on-device SKU from a Glean-class vendor |
| Model licensing terms for bundled models | Quarterly | Legal | Any change affecting LIC-001 |
| MCP ecosystem direction | Per spec release | Eng | Any change to §52 corrections |

---

# 83. Go-to-market and distribution **[ASSUMPTION]**

## 83.1 Motion sequence

| Stage | Segment | Why this order | Ships after |
|---|---|---|---|
| 1 | Developers and technical prosumers | They tolerate a first-index period, they have local corpora that matter (repos + docs), and they evaluate action safety seriously. The revised P3-before-P4 ordering (§53.2) matches this beachhead exactly. | P2 |
| 2 | Broader prosumer / knowledge worker | Requires rich formats and lower friction — Office parsing, OCR, better onboarding | P3 + parts of P6 |
| 3 | Team | Requires policy templates and seat management, not a server | P7 |
| 4 | Enterprise / regulated | Requires managed policy, audit, private inference, and a completed security audit (§86) | P7 |

**Do not attempt stage 4 before stage 1 is proven.** Enterprise diligence will ask for the audit, the SOC 2 report and the injection test results (§56 P3 gate). Arriving without them wastes the pipeline.

## 83.2 Distribution channels

| Channel | Fit | Blocker | Decision |
|---|---|---|---|
| Direct download, signed + notarized (PKG-007) | ✅ Primary | None | V1 |
| Homebrew cask / winget | ✅ Strong for stage 1 | None | V1 |
| Mac App Store | ⚠️ | Full App Sandbox conflicts with the §51 V1 posture (non-sandboxed parser subprocess) and with the daemon model; security-scoped bookmarks are workable but the subprocess story is not | **Not V1.** Revisit after V2 sandbox posture |
| Microsoft Store | ⚠️ | MSIX packaging constrains the parser subprocess and background daemon similarly | Revisit at P6 |
| Setapp / bundle marketplaces | ~ | Revenue share; audience mismatch with BYO-key | Evaluate at stage 2 |
| Linux: Flatpak / AppImage | ~ | Flatpak sandbox interacts with folder consent and Landlock | AppImage first; Flatpak at P6 |
| Enterprise MSI / MDM (Intune, Jamf) | Required for stage 4 | Needs MULTI-012 managed policy | P7 |

## 83.3 Launch gates — do not ship publicly before these

| Gate | Source |
|---|---|
| Zero placeholder hydration in a 72-hour soak on a real OneDrive/iCloud account | §56 P1, R3 |
| Watcher reconciliation drift = 0 over 72 hours | §56 P1 |
| Adversarial injection corpus: **0 escapes** | §56 P3, R6 |
| Every mutation has a declared reversibility class and a validator | §56 P3 |
| Sandbox posture documented accurately and matching reality | §51, checklist item 36 |
| Crash-free session rate ≥ 99.5% in beta | §84 |
| Accessibility: approval dialogs screen-reader complete | A11Y-002 |
| Uninstall does not delete another user's data | MULTI-010 |
| Diagnostic bundle contains no file bodies and no other user's paths | §23.3, MULTI-014 |

## 83.4 Proof artifacts required at launch

| Artifact | Why |
|---|---|
| Reproducible benchmark report (scan, lexical p95, hybrid p95) on named hardware | The performance claims in §28 are the first thing a technical audience tests |
| Public security whitepaper stating the **actual** sandbox posture | §51. Overstating it once destroys the trust the whole product is built on |
| Provenance demo: an answer, clicked through to a PDF page and a spreadsheet cell | Differentiator #1 is only credible when shown |
| Prompt-injection demo: a hostile README that fails to do anything | Differentiator #4, made visible |
| Export bundle demo | Proves the "no hostage" claim (ENT-006, SYNC-002) |

## 83.5 Metrics that matter

| Metric | Definition | Why not the obvious one |
|---|---|---|
| **Activation** | First cited answer opened to its source | Installs and index-completion measure nothing about value |
| Time-to-first-useful-result | Install → first successful search | Directly tests Journey A (§39) |
| Retention (D30 query rate) | Users running ≥ 3 queries in week 4 | |
| **Correction rate per extraction type** | EVAL-007 | The only production quality signal available without seeing content |
| Approval friction | Approvals per successful action | Rising values mean the risk model is miscalibrated, not that users are careless |
| Abstention correctness | Sampled, on internal corpus only | Users cannot report a hallucination they did not notice |
| Support contacts per 100 active users | | Drives §80.4 margin directly |

## 83.6 Launch anti-patterns

| Anti-pattern | Why |
|---|---|
| Demoing the knowledge graph as a hairball | §16.5 explicitly rejects it; it also over-promises P4 during a P2 launch |
| Claiming "sandboxed" in marketing while §51 V1 posture is in effect | Verifiable, and fatal to a security-positioned product |
| Leading with "AI" rather than provenance | Puts us in a category where C4 wins on price and distribution |
| Publishing a benchmark without the corpus definition | Invites a credibility fight we would lose |
| Shipping cloud enrichment defaults on | Violates §48; generates a bill and a battery complaint on day one |

---

# 84. Support and escalation — `SUP` **[DERIVED]**

Support is the dominant variable cost (§80.4) and the primary source of the only production quality signal we permit ourselves (EVAL-007). It is designed here as a subsystem, not an afterthought.

## 84.1 The structural constraint

> **Support can never ask to see the user's files, index, queries or entity labels.** Every workflow below is designed around that.

This is not a courtesy. It follows from TEL-002 and §23.3 and is the reason the diagnostic bundle exists.

## 84.2 Tiers

| Tier | Audience | Channel | Target first response |
|---|---|---|---|
| T0 — Self-serve | All | In-app diagnostics, docs, index-health explanations (§16.10) | Immediate |
| T1 — Community | Free | Forum, GitHub issues | Best effort |
| T2 — Email | Pro | Email + attached diagnostic bundle | 2 business days |
| T3 — Priority | Team / Enterprise | Email + scheduled call | 8 business hours |
| T4 — Engineering escalation | Any severity S1/S2 | Direct to on-call | Per §84.3 |

## 84.3 Severity

| Sev | Definition | First response | Target resolution |
|---|---|---|---|
| **S1** | Data loss; an unauthorized or unapproved mutation executed; index corruption affecting many users; any confirmed security incident (§85) | 1 hour | Workaround 24 h, fix in next release train |
| **S2** | Core capability unusable (search returns nothing, indexing never completes, app will not launch); cloud egress occurred against policy | 8 business hours | 5 business days |
| **S3** | Degraded capability with a workaround; parser failures on a format; performance regression | 2 business days | Next minor |
| **S4** | Cosmetic, docs, feature request | 5 business days | Backlog |

**S1 special rules:** an unapproved mutation or a policy-violating egress is S1 **even if a single user is affected and no data was lost**. Those are correctness failures of the security core, not support tickets.

## 84.4 Diagnostic-first workflow

```text
User reports issue
   |
   v
In-app "Create diagnostic bundle"  (§23.3)
   |
   v
User reviews bundle contents in a viewer   <-- TEL-004 principle applied to support
   |
   +--> user redacts or cancels
   |
   v
User attaches bundle to ticket
   |
   v
Support reproduces against the internal permissioned corpus (EVAL-001)
   |
   +-- reproduced --> engineering, with a corpus fixture added to CI
   |
   +-- not reproduced --> request a targeted counter/log level, never content
```

## 84.5 Requirements

| ID | Requirement |
|---|---|
| SUP-001 | Every error surfaced to the user names a cause and an action, not a code alone. Index-health errors link to the specific root and file class (§16.10). |
| SUP-002 | Diagnostic bundle generation is one click from the error, and is viewable before sending. |
| SUP-003 | Bundles contain no file bodies, no full paths by default (path shapes and extensions only), no queries, no entity labels, no other user's data. |
| SUP-004 | Support staff have **no** mechanism to request an index, a database or raw content. The capability does not exist in the product. |
| SUP-005 | No remote-control, screen-share-driven configuration, or remote debugging capability ships in the product. |
| SUP-006 | Every reproduced defect adds a fixture to the internal corpus (EVAL-002) and a regression test. |
| SUP-007 | Known issues are published with symptom, affected versions, workaround and status. |
| SUP-008 | A user-visible changelog states security-relevant fixes explicitly. |
| SUP-009 | Support tooling stores ticket text under the same retention policy as customer data and is covered by the DPA (§87). |
| SUP-010 | Escalation to security (§85) is available to any support tier without gatekeeping. |
| SUP-011 | Free-tier deflection is by documentation and in-app explanation quality, never by hiding the diagnostic bundle. |
| SUP-012 | Support volume by category is reported monthly against the §80.4 cost model. |

## 84.6 Deflection design (this is the margin lever)

| Cause of contact | Deflection |
|---|---|
| "Indexing never finishes" | Index-health card with per-stage counts, backlog, and the §48 governor state made visible (§16.10) |
| "It didn't find my file" | "Why not found" explanation: excluded by policy / cloud-only (TIER-008) / parse failed / not yet embedded |
| "It's using my battery" | Visible scheduler state (§21) and a one-click pause (WS-007, UX-014) |
| "Answer was wrong" | Citation click-through plus the correction UX (§16.6) — converts a ticket into an EVAL-007 datapoint |
| "Is my data being uploaded?" | Persistent privacy indicator (UX-012) and egress disclosure (UX-013) |
| "Installer is huge" | Pre-download size disclosure (PKG-013) |

---

# 85. Security incident response and disclosure — `SIR` **[DERIVED + COUNSEL]**

## 85.1 Incident classes specific to this product

| Class | Example | Sev | Note |
|---|---|---|---|
| I1 | Indirect prompt injection reached a mutation or an egress | **Catastrophic** | R6/R18. The single failure the architecture exists to prevent |
| I2 | Content of a `RESTRICTED`/`LOCAL_ONLY` workspace egressed to a cloud provider | Catastrophic | §27 classification failure |
| I3 | Cloud placeholder mass hydration (R3) | Severe | User-visible as a bandwidth/quota event, and a trust event |
| I4 | Cross-user index or knowledge exposure | Severe | MULTI failure |
| I5 | RCE via codec, PDF, archive or image parser (R19) | Severe | Sandbox posture failure |
| I6 | Supply chain: compromised model, Tree-sitter grammar, Python sidecar dep, or MCP server | Severe | LIC/CONV-011/MCP-017 surface |
| I7 | Credential exposure (provider key, licence, OAuth token) | Severe | SEC-005 failure |
| I8 | Index or transaction-snapshot corruption causing data loss | High | §47.4 snapshots are user data |
| I9 | Update channel compromise | Catastrophic | Signing key management |

## 85.2 Process

| Phase | Actions | Clock |
|---|---|---|
| Detect | Report intake (§85.3), crash signal, or internal finding | T0 |
| Triage | Assign class + severity; page on-call security | ≤ 1 h for I1/I2/I9 |
| Contain | Determine whether a mitigation exists without an update (§85.4) | ≤ 4 h |
| Assess | Which versions, which platforms, is exploitation observed, is user data affected | ≤ 24 h |
| Remediate | Fix, test against the adversarial corpus (§29.4, §71.5), release out-of-band if needed | Per severity |
| Notify | Users, and regulators where required | **[COUNSEL]** — statutory clocks (e.g. 72 h under GDPR Art. 33 where applicable) must be confirmed by counsel per jurisdiction |
| Post-mortem | Blameless; adds a test to the adversarial corpus; updates the threat model (§38) | ≤ 10 business days |

## 85.3 Coordinated disclosure

| Item | Position |
|---|---|
| Published policy | `security.txt` + a documented disclosure page with a PGP key |
| Safe harbour | Explicit good-faith research safe harbour |
| Target acknowledgement | 3 business days |
| Target fix | 90 days, negotiable; researcher credited unless they decline |
| Bounty | Not at launch. Introduce **before** enterprise GTM (stage 4), scoped to I1/I2/I5/I9 |
| Scope note | The adversarial corpus (§29.4) is the specification of what we consider a vulnerability in the agent layer. Publish it. It is a differentiator (§82.2 #4), not a liability. |

## 85.4 Mitigation without an update — and its limits

A local-first product must be careful here. A remote kill switch is itself an attack surface and contradicts the product's promise.

| Mechanism | Available to | Position |
|---|---|---|
| Ship an out-of-band signed update | All | ✅ Primary mechanism |
| Local feature flags in config, user-controlled | All | ✅ Supported |
| Enterprise managed policy disabling a capability (§22.1) | Enterprise only | ✅ Signed, machine-scoped, transparent to the admin |
| Vendor-initiated remote disable of a capability on consumer devices | — | ✗ **Not implemented.** The capability does not exist. |
| Revoking a model or grammar from the download service | All | ✅ Prevents new installs; existing installs need an update |

## 85.5 Requirements

| ID | Requirement |
|---|---|
| SIR-001 | Published vulnerability disclosure policy and `security.txt` before public launch. |
| SIR-002 | Signing keys (app, licence, policy, model manifest) held in an HSM or equivalent, with documented rotation and a break-glass procedure. |
| SIR-003 | Update channel integrity: signed manifests, pinned keys, rollback protection, and PKG-009 (no update during an open transaction). |
| SIR-004 | Model, grammar and sidecar-dependency manifests are content-addressed and verified before use (PKG-011, CONV-011). |
| SIR-005 | Every I1-class finding adds a permanent case to the adversarial corpus, run in CI thereafter (§56 P3 gate). |
| SIR-006 | Security-relevant fixes are named as such in the changelog (SUP-008). |
| SIR-007 | An SBOM is generated per release, covering Rust crates, the Python sidecar tree, and bundled native libraries. |
| SIR-008 | CVE monitoring on the Rust dependency tree, the Python sidecar tree (R16), ffmpeg, PDFium and ONNX Runtime, with a defined patch SLA per severity. |
| SIR-009 | External penetration test and agent-safety audit before the first release that enables mutations (P3), and again before enterprise GTM. |
| SIR-010 | Incident communications never require, request or accept user file content. |

---

# 86. Compliance and certification path — `CMP` **[COUNSEL]**

> Everything in §86–§88 is a structured checklist for qualified counsel and an assessor. It is not legal advice and must not be treated as a compliance determination.

## 86.1 The key structural insight

Because processing happens on the user's device and the vendor never receives file content, LKAR's compliance surface is **far smaller than a SaaS product of equivalent capability — but it is not zero.**

| Component | Vendor receives | Vendor's likely role | In certification scope |
|---|---|---|---|
| Desktop app: indexing, retrieval, graph, actions | **Nothing** | Not a processor of file content | Product security (SDLC), not service scope |
| Opt-in telemetry (TEL-003) | Counters, latencies, crash traces | Controller (of that limited data) | ✅ |
| Crash reporting | Stack traces, sanitized | Controller | ✅ |
| Update / model download service | IP address, version, platform | Controller | ✅ |
| Licence and activation service | Account identifier, seat state | Controller | ✅ |
| Support desk | Ticket text, diagnostic bundles | Controller | ✅ |
| **Enterprise managed-inference add-on** (§22.2, §80.2) | **Retrieved context and prompts** | **Processor** | ✅ **Highest scrutiny** |
| BYO-key cloud inference | **Nothing — traffic is user → provider** | Neither | See §87.1 |

**Consequence:** the managed-inference add-on is the single component that converts LKAR from a low-compliance product into a high-compliance one. Price and scope it with that in mind, and keep it optional and separable.

## 86.2 GDPR / UK GDPR checklist

| Item | Position | Status |
|---|---|---|
| Role determination per component | §86.1 table | Draft — needs counsel sign-off |
| Lawful basis for telemetry | Consent (TEL-001, opt-in, revocable) | Aligns with architecture |
| Lawful basis for licence/update | Contract necessity | |
| Data minimisation | TEL-002 prohibits content, paths, queries, entity labels | Architecturally enforced |
| Purpose limitation | Telemetry used only for quality; never for profiling or marketing | Must be stated in the policy |
| **Right to erasure** | For vendor-held data: standard DSR. For on-device data: §26 "forget" already implements it, user-executed | Strong position |
| Right of access / portability | SYNC-002 export bundle for on-device data | Strong position |
| Records of processing (Art. 30) | Required for the vendor-side services | To do |
| DPIA | Likely required for the managed-inference add-on; arguably not for the local-only product | Counsel |
| International transfers | SCCs / IDTA for any non-EU subprocessor (§87) | Counsel |
| Breach notification | §85.2 | Clocks confirmed by counsel |
| **Special category data** | Users will index health, legal and financial documents. The vendor never receives them; the *product* must not create new risk (META-003 GPS default off, IMG-015 no face recognition, AUD-009 no voice ID) | Architecturally addressed |

## 86.3 CCPA / CPRA

| Item | Position |
|---|---|
| Sale / sharing of personal information | **None.** No ad or data monetisation (§80.1) — state this affirmatively |
| Categories collected | Telemetry, crash, licence, support only |
| Opt-out mechanism | Telemetry off by default (TEL-001) makes this largely moot |
| Sensitive PI | Not collected by the vendor |

## 86.4 SOC 2 Type II

| Item | Detail |
|---|---|
| **Scope** | Vendor-operated services only: update/model CDN, licence service, telemetry ingest, crash ingest, support desk, plus SDLC and corporate IT. **Not** the customer's device. |
| Trust services criteria | Security (required); Availability and Confidentiality if enterprise deals demand them; Privacy only if managed inference ships |
| Readiness work | 3–5 months (policies, access control, vendor management, change management, logging, onboarding/offboarding) |
| Observation window | 6–12 months (Type II) |
| Realistic first report | ~12–15 months after starting, i.e. **start readiness no later than the beginning of P5** to have a report for stage-4 GTM |
| Cost **[ASSUMPTION]** | $25–60k audit + $15–40k tooling/year + meaningful internal time |
| Trap to avoid | Scoping the desktop app into the report. It invites an assessor to opine on things the report cannot cover and confuses enterprise buyers. Publish the security whitepaper (§83.4) for the product, and the SOC 2 for the services. |

## 86.5 ISO 27001

| Item | Detail |
|---|---|
| When | Only when European or public-sector enterprise deals require it — commonly instead of, not alongside, SOC 2 |
| Overlap | ~70% control overlap with SOC 2; sequence SOC 2 first if the pipeline is US-weighted |
| Timeline | 9–14 months from start; Stage 1 + Stage 2 audit |
| Cost **[ASSUMPTION]** | $30–70k first cycle |

## 86.6 Regulated verticals

| Regime | What the local-first architecture buys | What it does not |
|---|---|---|
| HIPAA | If the vendor never receives PHI, no BAA is needed for the local product. Strong position. | A BAA **is** required for managed inference. Air-gapped SKU is the clean answer for covered entities. |
| FedRAMP | Not applicable to on-device software; applies to the vendor's cloud services if a federal customer uses them | An air-gapped deployment with telemetry hard-off and offline activation sidesteps most of it |
| PCI DSS | Out of scope; the product never touches cardholder data | Payment processing is delegated to a compliant processor |
| Financial (SEC 17a-4 etc.) | Not a records system; do not position it as one | Explicitly disclaim retention/archival guarantees |
| **Export control** | Encryption at rest (SEC-006) and TLS make this a real item | Classification (e.g. ECCN 5D002 / mass-market determination) required before international distribution — see §88 |

## 86.7 EU AI Act positioning **[COUNSEL]**

| Question | Working position |
|---|---|
| Are we a provider of an AI system? | Yes, for the integrated system, even though the models may be third-party or user-supplied |
| Risk classification | Likely **limited risk** → transparency obligations. Not an Annex III high-risk use case in the default product |
| What could change that | Any positioning toward employment screening, creditworthiness, education assessment or law enforcement use. **Do not market into those.** |
| Transparency obligations already met | UX-006 (found / inferred / user-confirmed), IMG-008 (AI-generated content marked), UX-012 (execution boundary), §12.4 (citations) |
| GPAI obligations | Fall on the model provider, not on us, when the user supplies the model. Bundled models must have documentation retained (LIC-002) |
| Action item | Formal assessment before EU marketing; keep IMG-015 / AUD-009 non-goals, which keep biometric categories out of scope entirely |

## 86.8 Accessibility compliance

| Regime | Requirement | Mapped to |
|---|---|---|
| EN 301 549 / European Accessibility Act | Applies to consumer software sold in the EU | A11Y-001..006 |
| ADA / Section 508 | US public-sector procurement | A11Y-001..006 + VPAT |
| Action | Produce a **VPAT** before stage-3 GTM; approval-dialog screen-reader completeness (A11Y-002) is a launch gate (§83.3) | |

## 86.9 Requirements

| ID | Requirement |
|---|---|
| CMP-001 | Maintain a component-to-role register (§86.1) and review it whenever a new vendor-side service is added. |
| CMP-002 | Any new vendor-side data collection requires a documented purpose, lawful basis and retention period before it ships. |
| CMP-003 | Telemetry and crash payload schemas are versioned, reviewed, and testable against TEL-002 in CI. |
| CMP-004 | Retention periods are defined and enforced for every vendor-held datastore. |
| CMP-005 | Managed inference, if shipped, is a **separately scoped** service with its own DPA, subprocessor list and retention policy. |
| CMP-006 | The privacy policy states, in plain language, that file content never reaches the vendor in the default product. |
| CMP-007 | Provider retention behaviour is surfaced in the privacy UI (LIC-007, §26.3) and kept current. |
| CMP-008 | Records of processing (Art. 30) maintained for vendor-side services. |
| CMP-009 | DSR workflow documented with an owner and an SLA. |
| CMP-010 | Subprocessor register published and versioned (§87.2). |
| CMP-011 | SOC 2 readiness begins no later than the start of P5. |
| CMP-012 | VPAT produced and maintained per release train. |
| CMP-013 | Export-control classification completed before international distribution. |
| CMP-014 | SBOM (SIR-007) retained per release for the supported version window. |
| CMP-015 | Compliance artifacts are reviewed at each phase gate (§56), not only before enterprise deals. |

## 86.10 Sequencing

| Phase | Compliance work |
|---|---|
| P1 | Privacy policy, EULA, OSS attribution/NOTICE, telemetry schema review, export-control classification |
| P2 | DSR workflow, records of processing, subprocessor register, provider retention disclosure |
| P3 | External security audit (SIR-009), VPAT, vulnerability disclosure policy |
| P4–P5 | SOC 2 readiness starts; policy and control implementation |
| P6 | SOC 2 observation window; second security audit |
| P7 | SOC 2 Type II report available; ISO 27001 if pipeline demands; DPA templates finalised; EU AI Act assessment |

---

# 87. Data processing agreements and subprocessors **[COUNSEL]**

## 87.1 The BYO-key wrinkle — architecturally important

When a user supplies their own provider key (the default in Free/Pro/Team, §80.2):

| Fact | Consequence |
|---|---|
| Traffic goes **device → provider**, never through vendor infrastructure | The vendor is not a processor of that content |
| The contract for that inference is **user ↔ provider** | Provider ToS, retention and training policies are the user's to accept |
| The vendor cannot make retention promises about it | §26.3 already says this. The UI must say it too, at key-entry time |
| Enterprise buyers will still ask us to warrant it | We cannot. Correct answer: managed inference (§86.1) where we *are* the processor and *can* contract |

| ID | Requirement |
|---|---|
| DPA-001 | At provider-key entry, the UI states plainly that the user's agreement with that provider governs the data, and links to that provider's retention policy. |
| DPA-002 | The privacy policy distinguishes vendor-processed data from user-directed provider traffic. |
| DPA-003 | Marketing never implies vendor control over BYO-key provider retention. |

## 87.2 Subprocessor register

| Function | Data | Notes |
|---|---|---|
| Update / model CDN | IP, user agent, version | Minimise logs; short retention |
| Licence / activation | Account ID, seat state | No content, ever |
| Crash reporting | Sanitized traces | Must be scrubbed of paths (TEL-002) before transmission, not after |
| Telemetry ingest | Counters, histograms | Opt-in only |
| Support desk | Ticket text, diagnostic bundles | Bundle contents constrained by SUP-003 |
| Email / billing | Contact, payment metadata | Payment delegated to a compliant processor |
| Managed inference (Enterprise, optional) | **Prompts and retrieved context** | Highest scrutiny; separate DPA; separate retention |

| ID | Requirement |
|---|---|
| DPA-004 | Published, versioned subprocessor list with a change-notification mechanism for Enterprise. |
| DPA-005 | A DPA is executed with every subprocessor before it handles any customer data. |
| DPA-006 | Transfer mechanism (SCCs / IDTA / adequacy) documented per subprocessor. |
| DPA-007 | Each subprocessor's retention and deletion commitments are recorded and reconciled annually. |
| DPA-008 | Enterprise contracts offer: DPA, TOMs description, subprocessor notice, audit rights (report-based), breach notification terms, and insurance evidence. |
| DPA-009 | No subprocessor may be added to a path that carries file content without an explicit compliance review (CMP-002). |
| DPA-010 | Crash and telemetry scrubbing is verified by test against real payloads before each release. |

---

# 88. Legal and policy artifact checklist **[COUNSEL]**

| Artifact | Needed by | Note |
|---|---|---|
| EULA | P1 launch | Include the honest capability statement (§78.3) by reference |
| Terms of Service (for vendor-side services) | P1 launch | |
| Privacy Policy | P1 launch | Must reflect §86.1 role split |
| OSS attribution / NOTICE file | P1 launch | LIC-005; generated in CI from the SBOM |
| Model licence notices | P1 launch | LIC-002/LIC-004 |
| Tree-sitter grammar licence audit | P1 launch | LIC-006 — audited individually; licences vary by grammar |
| ffmpeg licensing statement (LGPL build) | P6 | VID-010 |
| Python sidecar dependency licence audit | P2 | CONV-011 |
| **Export-control classification** | Before international distribution | Encryption at rest + TLS; commonly a mass-market determination, but must be filed/classified properly |
| Security whitepaper (accurate sandbox posture) | P1 launch | §51, §83.4 |
| Vulnerability disclosure policy + `security.txt` | P1 launch | SIR-001 |
| Accessibility statement + VPAT | P3 | CMP-012 |
| DPA template | P7 (earlier if Team sells) | DPA-008 |
| Subprocessor list | P2 | DPA-004 |
| Records of processing | P2 | CMP-008 |
| DPIA (managed inference) | Before that add-on ships | CMP-005 |
| EU AI Act assessment | Before EU marketing | §86.7 |
| Insurance (E&O / cyber) | Before enterprise GTM | DPA-008 |

---

# 89. Total cost model — company, not just engineering **[ASSUMPTION]**

Extends §54.1, which covered engineering only.

| Line | V1 (≈12 mo) | Through P4 (≈28 mo) | Full (≈45 mo) |
|---|---|---|---|
| Engineering (§54.1, Part 3 revised §74.2) | ~$640k | ~$1.9M | ~$3.2M |
| Design / UX (0.5–1 FTE) | ~$110k | ~$260k | ~$420k |
| Product / PM (0.5–1 FTE) | ~$90k | ~$260k | ~$430k |
| QA / eval corpus construction | ~$80k | ~$220k | ~$380k |
| Security audits (SIR-009, ×2) | — | ~$90k | ~$180k |
| Compliance (SOC 2 readiness, audit, tooling) | — | ~$60k | ~$160k |
| Legal (§88 artifacts, DPAs, export, AI Act) | ~$40k | ~$110k | ~$200k |
| Support (scaling with users) | ~$40k | ~$260k | ~$700k |
| GTM / marketing | ~$80k | ~$400k | ~$900k |
| Infrastructure (CDN, licence, telemetry, CI) | ~$25k | ~$90k | ~$180k |
| Code signing, notarization, dev programs | ~$15k | ~$35k | ~$60k |
| **Total** | **~$1.1M** | **~$3.7M** | **~$6.8M** |

| Observation |
|---|
| Engineering is ~55% of the full-spec total, not ~100%. The §54 figure understates the company cost by roughly 2×. |
| Support and GTM together exceed the cost of the entire graph phase. This is the argument for §84.6 deflection design being an engineering priority, not a support-team priority. |
| The V1 line is the decision-relevant number: **~$1.1M to a shippable, differentiated product** (P0–P2), assuming the §79.2 Free/Pro split. |

---

# 90. New open decisions

| # | Decision | Blocked by | Deadline | Recommendation |
|---|---|---|---|---|
| D19 | Free-tier boundary: P1 (search) or P2 (semantic) | Beta conversion data | Before P2 exit | P1 — the free tier is the distribution mechanism |
| D20 | Does the Team SKU exist, or collapse into Pro? | Pricing research | Before P3 | Collapse unless workspace lock + policy templates test as a real buying trigger |
| D21 | Perpetual licence offered alongside subscription? | Revenue modelling | Before P2 | Yes for Pro only, priced at ≥ 20 months |
| D22 | Managed inference add-on: build or refuse? | Enterprise pipeline evidence | Before P7 | Refuse until an enterprise deal requires it — it is the entire compliance surface (§86.1) |
| D23 | SOC 2 vs ISO 27001 first | Pipeline geography | Start of P5 | SOC 2 if US-weighted |
| D24 | Bug bounty: launch it, and at what scope | Security maturity | Before stage-4 GTM | Yes, scoped to I1/I2/I5/I9 |
| D25 | Mac App Store / MSIX distribution | V2 sandbox posture (§51) | End P6 | Direct-only for V1 |
| D26 | Workspace lock (MULTI-009): all tiers or paid? | §79.3 tension — it is arguably a safety feature | Start P3 | Paid, but shared-account **warning** (MULTI-008) is free |
| D27 | Open-core: which crates, if any | Strategy | P7 | Consider parsers and IR; never the policy engine |
| D28 | Telemetry vendor: self-host or third-party | Compliance surface | P2 | Self-host — it removes a subprocessor from a sensitive path |
| D29 | Support tooling vendor and its data residency | Compliance | P2 | Choose EU-capable residency to avoid a transfer analysis |
| D30 | Do we publish the adversarial corpus (§29.4)? | Security posture | Before P3 launch | Yes — it is differentiator #4 made verifiable |

## 90.1 Also resolved here

| From | Item | Resolution in this part |
|---|---|---|
| §59 D10 | Business model / pricing | §80 — freemium + per-seat, BYO inference **[ASSUMPTION, validate before P2]** |
| §59 D11 | Competitive positioning | §82.4 — provenance and action safety, not "AI for files" |
| §60 | Multi-user behaviour | §78 — `MULTI` block, P1 |

---

# 91. New risks

| # | Risk | Prob | Impact | Mitigation |
|---|---|---|---|---|
| R23 | Shared-account (S3) leakage becomes a public trust incident | Low | **Severe** | MULTI-008 honesty in onboarding; MULTI-009 workspace lock; never claim per-human isolation |
| R24 | Index on a redirected/network profile corrupts (S6) | Medium | High | MULTI-007 detection and local-disk fallback; refuse rather than degrade silently |
| R25 | Free tier cannibalises Pro | Medium | High | D19 measured in beta; move the boundary to actions rather than crippling search |
| R26 | BYO-key friction caps the addressable market | Medium | Medium | Strong local model default; 30-second key flow |
| R27 | Enterprise demands managed inference, which imports the full SaaS compliance surface | Medium | High | D22 — refuse until contractually justified; price it to cover §86 cost |
| R28 | Support cost per user exceeds the §80.4 model | Medium | High | §84.6 deflection treated as an engineering deliverable with a monthly metric |
| R29 | An OS vendor ships provenance-grade citation | Low | **Severe** | §82.5; monitored monthly; fall back to action safety and model neutrality as primary differentiation |
| R30 | Export-control classification blocks or delays international release | Low | Medium | CMP-013 completed in P1, not at launch |
| R31 | Marketing overstates the sandbox posture | Medium | **Severe** | §83.6; security whitepaper reviewed by engineering before every campaign |
| R32 | SOC 2 scope creeps to include the desktop app | Medium | Medium | §86.4 scoping decision documented and defended with the assessor |

---

# 92. Summary of what Part 4 changed

| Area | Before | After |
|---|---|---|
| Multi-user / shared machine | Deferred "edge case", P3 | `MULTI` block, 15 requirements, **8 in P1**; identified as a schema and IPC concern, not a UX one |
| Business model | Not addressed (D10 open) | Freemium + per-seat, BYO inference, COGS ≈ $0; 5 SKUs; explicit never-gated list |
| Pricing | Not addressed | Indicative points, unit economics, trial mechanics — all tagged as requiring validation |
| Entitlement | Not addressed | `ENT` block, 12 requirements; offline-first; **never locks the user's own data** |
| Competitive | Not addressed (D11 open) | 7-category map, ranked differentiators, honest loss list, moat-durability analysis, monitoring cadence |
| GTM | Not addressed | 4-stage motion, channel decisions, 9 launch gates, 5 proof artifacts, metric definitions |
| Support | Not addressed | `SUP` block, severity model, diagnostic-first workflow, deflection design identified as the **margin lever** |
| Security IR | Partially implied | `SIR` block, 9 incident classes, disclosure policy, and an explicit refusal to build a consumer remote kill switch |
| Compliance | Not addressed | `CMP` block; role-split analysis showing managed inference is the whole compliance surface; SOC 2 scoping trap named; EU AI Act and export control added |
| DPAs | Not addressed | `DPA` block; the BYO-key architectural wrinkle made explicit |
| Cost model | Engineering only (~$2.7–3.2M) | Company total **~$6.8M** full spec; **~$1.1M to V1** |
| Open decisions | D1–D18 | + D19–D30; D10 and D11 resolved |
| Risks | R1–R22 | + R23–R32 |

## 92.1 New requirement blocks

| Prefix | Topic | Count | Phase |
|---|---|---|---|
| `MULTI` | Multi-user and shared machine | 15 | **P1** (8) / P3 (7) |
| `ENT` | Entitlement and licensing | 12 | P2 |
| `SUP` | Support and escalation | 12 | P1→ |
| `SIR` | Security incident response | 10 | P1→ |
| `CMP` | Compliance | 15 | P1→ |
| `DPA` | Data processing and subprocessors | 10 | P2→ |
| **Total added** | | **74** | |

Running requirement total across Parts 1–4: **~423**.
