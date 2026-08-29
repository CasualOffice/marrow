---
name: spec
description: Navigate the 7-part LKAR specification — find requirements by ID, resolve which part supersedes which, and locate the section covering a subsystem. Use whenever a question needs an answer from the spec, a requirement ID appears (like TIER-005, CHK-002, EXEC-007), or you are about to implement something the docs already specify.
---

# Navigating the LKAR spec

~7,900 lines across seven parts in `docs/`. Written in order over time, each part correcting the last.

## The one rule

**Later parts supersede earlier ones.** They exist to correct the earlier ones.

- **Part 7 supersedes Part 4 entirely** (commercial → solo re-scope)
- Part 7 also amends Part 5 §97 (execution) and §95 (hardware), and Part 2 §51 (sandbox)
- Part 6 supersedes the data-model and API *sketches* in Part 1 §9, §8.12, §7.2, §30

**Never quote Part 1–6 guidance without checking whether Part 7 §124 dropped it or §126 kept it.**

## Where things live

| Looking for | Part | Sections |
|---|---|---|
| Product thesis, design principles | 1 | §1, §1.2 |
| Trust model, injection defence, path security | 1 | §6 |
| Component architecture | 1 | §8 |
| Ingestion flows | 1 | §10 |
| Extraction tiers, authority classes, contradictions | 1 | §11 |
| Query modes, ranking, context | 1 | §12 |
| Risk classes, transaction lifecycle | 1 | §14 |
| ADRs 1–10 | 1 | §33 |
| **Cloud placeholders (TIER)** | 2 | §45.1 |
| **Watcher limits (WATCH)** | 2 | §45.2 |
| **Verification subsystem (4 layers)** | 2 | §46 |
| **Reversibility classes** | 2 | §47 |
| **Tier C budget governor** | 2 | §48 |
| SQLite write architecture | 2 | §50 |
| Risk register R1–R14 | 2 | §55 |
| **Parser tiers T1–T5, provenance classes** | 3 | §63 |
| OCR, images, video, audio | 3 | §65–68 |
| **Embedded metadata (META)** | 3 | §69 |
| Multimodal threats | 3 | §71 |
| ~~Commercial, compliance~~ | 4 | **superseded** |
| Prior-art analysis (13 systems) | 5 | §94 |
| Hardware probe, local LLM | 5 | §95–96 |
| **Execution tiers E0–E4** | 5 | §97 |
| **Self-poisoning rule** | 5 | §98.4 |
| **File intelligence, tables** | 5 | §99 |
| Agent parity, answer coverage | 5 | §100–101 |
| **Full SQLite DDL** | 6 | §106 |
| Errors, config, IPC | 6 | §108–110 |
| **Chunking, fusion, context envelope** | 6 | §112–114 |
| Test strategy, adversarial corpus | 6 | §116 |
| Glossary | 6 | §120 |
| **Solo re-scope: what's cut** | 7 | §124 |
| **What must NOT be cut** | 7 | §126 |
| **MCP-first inversion** | 7 | §130 |
| **Build plan** | 7 | §131 |

## Finding a requirement ID

IDs are `PREFIX-NNN` and permanent — never reused, never renumbered.

```sh
grep -rn "TIER-005" docs/          # find the requirement and every reference
grep -rn "^| TIER-" docs/          # the whole TIER block
```

Block index with counts and phases: **Part 6 §121.1**. Post-solo-trim status per block: **Part 7 §132**.

## Before implementing anything

1. Find the block that governs it (table above)
2. Check **Part 7 §132** — is it kept, reduced or dropped under solo scope?
3. Check **Part 7 §126** — is it one of the 14 non-negotiables?
4. Check **Part 6** for the concrete artifact (DDL, algorithm, contract)
5. Check the current milestone in `TRACKER.md` — is it even in scope yet?

## Traps

| Trap | Reality |
|---|---|
| Quoting Part 4 | Superseded. Nothing commercial is in scope |
| Assuming shell execution is disabled | Part 2 §51 said so; **Part 7 §129 reversed it** for solo use |
| Assuming a sandbox is needed | Settled: never building one |
| Assuming an agent runtime is needed | Part 7 §130: delegated to Claude Code via MCP |
| Assuming 100k-file scale targets | Part 7 §127: size against the author's real corpus (M0) |
| Building all 40 tables from §106 | ROADMAP "Schema staging" — M1 needs 11 |
| Treating the spec as a build order | It's a reference. `ROADMAP.md` is the build order |

## Reporting back

Cite as `Part N §M` and say when Part 7 changed the answer. If the spec is silent, say so — don't extrapolate a requirement that isn't there.
