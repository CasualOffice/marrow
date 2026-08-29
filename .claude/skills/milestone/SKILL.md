---
name: milestone
description: How to work a milestone in this repo — check current scope, apply the scope and stop rules, and update the tracker afterwards. Use when starting work, when deciding whether something is in scope, when a task is finished, or when tempted to build something the current milestone doesn't cover.
---

# Working a milestone

`ROADMAP.md` is the plan. `TRACKER.md` is the state and the file that gets updated.

## Start of any session

1. Read `TRACKER.md` — **Current milestone** and its open items
2. Read that milestone's section in `ROADMAP.md` for the exit criteria
3. Pick from the tracker. Don't invent work

## Is this in scope?

The spec is ~7,900 lines. The author is one person. **The default answer is no.**

```
Is it on the current milestone's tracker list?
├─ yes → do it
└─ no
   ├─ Does the current milestone's exit criterion require it?
   │  ├─ yes → do it, add it to the tracker first
   │  └─ no
   │     ├─ Is it a Part 7 §126 invariant? → do it, they're never deferred
   │     ├─ Does it block current work?     → do the minimum, note the debt
   │     └─ otherwise → TRACKER parking lot, say so, move on
```

**Specifically don't build:** a UI (D42) · the knowledge graph (D43) · an OS sandbox (settled: never) · an agent runtime, model gateway or approval UX (D-agent-layer) · parsers for files not yet encountered · abstractions for platforms, hardware tiers or users that don't exist.

## Scope rules

1. **Ship M2 before anything clever.** An index queried daily teaches more than planning does
2. **Add a parser the week a real file demanded it.** Never speculatively
3. **No UI until the CLI annoys you**
4. **No knowledge graph until D43 passes** — three real questions that needed it
5. **No abstraction for platforms you don't run**
6. **No performance work without a benchmark** against the author's own corpus

## Stop rules

- Milestone past **2× its estimate** → cut scope, don't extend time
- Three milestones deep with nothing used in a week → wrong milestone was built
- **S1 (scope collapse) is the top risk.** Every scope decision defaults to no until M2 ships

## Schema staging

Don't build all 40 tables from Part 6 §106. See `ROADMAP.md` → *Schema staging*. M1 needs 11.

**But** put `source_span` on `ir_nodes`, and `origin` / `content_hash` / `supersedes` on files and versions, from M1. Those are the retrofit-expensive ones.

## Finishing a task

1. Tick it in `TRACKER.md`
2. If something surprised you or contradicted the spec → add a `Log` entry, and write it into `DECISIONS.md` if it changes a choice
3. If you discovered work → add to the current milestone or the parking lot, don't just do it
4. Run the standing checks if you touched files, paths, evidence or writes

## Finishing a milestone

Exit gates from `ROADMAP.md`, plus these every time:

- [ ] Used for real, on the author's own corpus, for at least a week
- [ ] Tests green (adversarial corpus too, from M5 — non-negotiable)
- [ ] No secret in `settings.json` or any log
- [ ] Every derived index rebuildable from canonical state
- [ ] Corrections survive a full derived rebuild
- [ ] TRACKER updated: milestone marked done, dated, next one started
- [ ] No half-finished subsystem carried forward

## Estimates

`ROADMAP.md` gives (part-time / full-time) ranges. They're estimates. When one is wrong, **update the tracker with the actual** — the pattern of misses is more useful than any individual estimate.

## The milestone that matters

**M2 (MCP server) is the forcing function.** Everything before it is unproven; everything after is informed by real use. Keep it to 1–2 weeks — if it slips, cut tools, not time. If interest is going to fade, it fades before M2 (risk S7).
