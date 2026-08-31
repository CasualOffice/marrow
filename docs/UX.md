# Marrow — Experience Design

**Status:** Design. Precedes implementation of `marrow-cli` and `marrow-mcp`.
**Supersedes:** Part 1 §16 for the surfaces we actually build. §16 designs a desktop GUI; the GUI is deferred past M6 (D42). The surfaces that exist are **the terminal** and **MCP**.

---

## 1. Who is at the other end

Two consumers, and they want opposite things from the same data.

| Consumer | Wants | Optimise for |
|---|---|---|
| **A human in a terminal** | To find a thing, see why it matched, and jump to it | Scannability, first-result latency, jumpability |
| **An agent over MCP** | Structured evidence with stable handles it can cite | Determinism, explicit provenance, no prose |

**Rule:** one query engine, two renderers. The moment a renderer starts computing something the other can't get, the design is wrong.

---

## 2. Principles

| # | Principle | Consequence |
|---|---|---|
| 1 | **Never block fast results on slow ones** | Lexical hits stream immediately; semantic results arrive later and re-rank in place. No spinner ever gates a result that is already known (Part 1 §16.3) |
| 2 | **Every result is jumpable** | `path:line:col` — the format editors, IDEs and terminals already linkify. A citation you can't open is a screenshot of an answer |
| 3 | **Say why it matched** | `exact` · `semantic` · `path` · `recent`. Without this, hybrid ranking is a black box and the user stops trusting it |
| 4 | **Errors name a cause and an action** | Never a bare code. Every failure ends in a runnable command (SUP-001) |
| 5 | **Unknown is shown as unknown** | Absent metadata renders as `—`, never inferred, never blank-so-it-looks-fine |
| 6 | **Degraded provenance is visible** | An `~approx` badge on anything not `Exact`. Silent precision loss is the one thing that would destroy the product's premise |
| 7 | **The tool is quiet when it works** | No progress bars for sub-second work. No "✓ Done!". Output is the result, nothing else |
| 8 | **stdout is data, stderr is narration** | `marrow search x \| head` must work. Diagnostics never pollute the pipe |
| 9 | **Machine mode is not an afterthought** | `--json` on every command, same data, stable shape. It's how MCP is fed |
| 10 | **Latency is shown, not hidden** | A footer with time and branch used. It's how you notice a regression before a benchmark does |

---

## 3. Command surface

`marrow <verb>` for the things done many times a day. `marrow <noun> <verb>` for management. This is the shape `git`, `docker` and `gh` converged on because frequency should determine depth.

```
marrow search <query>          find things                     ← the hot path
marrow ask <question>          cited answer                    (M4+)
marrow file <path|id>          everything known about one file
marrow open <result-ref>       open a result in $EDITOR
marrow status                  index health

marrow workspace add <path>    grant a root
marrow workspace list
marrow workspace rm <name>
marrow workspace pause|resume
marrow workspace forget <name> delete derived knowledge

marrow index run               force a scan
marrow index reconcile         verify index against disk
marrow index rebuild <what>    rebuild a derived index

marrow job list|retry          the durable queue
marrow config get|set
marrow mcp                     serve over stdio               ← M2
marrow doctor                  diagnose, don't guess
```

### Global flags

| Flag | Behaviour |
|---|---|
| `--json` | Machine output. Stable schema, versioned. |
| `-w, --workspace <name>` | Scope. Defaults to all. |
| `-n, --limit <n>` | Default 20 for search. |
| `-q, --quiet` | Errors only. |
| `-v` / `-vv` | Progressively more narration **on stderr**. |
| `--no-color` | Also honours `NO_COLOR`, and auto-off when not a TTY. |
| `--explain` | Show ranking internals (RET-004). |

**No `--verbose` that changes stdout.** Verbosity is narration; data shape is a contract.

---

## 4. Search — the primary surface

```
$ marrow search "auth refresh token"

src/auth/token.rs:142                                     exact · 2d
  pub async fn refresh_token(&self, ctx: &Ctx) -> Result<Token> {
      let claims = self.decode(ctx.refresh)?;
  fn refresh_token › impl TokenService

docs/auth-design.md:88                                 semantic · 3w
  ## Refresh token rotation
  Tokens rotate on each use; the previous token is revoked after a
  Authentication › Refresh token rotation

notes/2026-06-standup.md:12                            semantic · 8w
  decided to move refresh tokens out of localStorage
  § June standup

17 results · 8 ms · lexical+semantic
```

### Anatomy

| Element | Why it earns its line |
|---|---|
| `path:line` on its own line, first | Jumpable, copy-pasteable, and the eye scans a left-aligned column of paths far faster than paths embedded in prose |
| Match reason, right-aligned | Principle 3. Right alignment keeps it out of the way of scanning paths |
| Age (`2d`, `3w`), not a date | Recency is what you're judging. `2026-06-14` makes you do arithmetic |
| 2–3 lines of matched content, indented | Enough to decide. More is a pager, not a search result |
| Breadcrumb, dimmed, last | `fn refresh_token › impl TokenService` or `Authentication › Refresh token rotation`. This is the **structural context prefix** the chunker already computes — surfacing it is free |
| Footer: count, time, branches used | Principle 10 |

### Progressive rendering

Results stream. Lexical hits appear in single-digit milliseconds; semantic results arrive later and are **merged in place** rather than appended, so the final order is the true ranking.

```
       t=8ms    3 lexical results printed
       t=140ms  semantic returns; list re-renders in place
```

In a TTY, re-render by rewriting the block. When piped, buffer and emit once — a pipe consumer must never see a partial ranking. If the terminal is too small to rewrite cleanly, degrade to append-only with a `+4 more after re-rank` note rather than corrupting the display.

### Zero results is a diagnosis, not a shrug

```
$ marrow search "quarterly revenue"

No matches.

  Indexed        9,435 files across 2 workspaces
  Not indexed    412 cloud-only files in "iCloud"     may contain this
                 66 xlsx hidden by .gitignore in "melp"

  → marrow workspace hydrate iCloud
  → marrow workspace set melp gitignore=off
```

This turns the single most common failure into a fixable one. It is also the deflection design from Part 4 §84.6, which survives the solo re-scope because *you* are the one debugging at 1am.

---

## 5. File intelligence

`marrow file` is the `FI` panel. Sections are ordered by how often you want them.

```
$ marrow file q2-report.xlsx

q2-report.xlsx
~/Desktop/finance/q2-report.xlsx                    14.1 MB · xlsx · 3w

IDENTITY
  file       01M17KJXJ2K51K…      stable since 12 Mar
  content    blake3:9f2a1c8e…     3 versions, 2 earlier paths
  duplicate  none

WHAT THIS FILE SAYS ABOUT ITSELF                          deterministic
  author         Sachin Sarwa            docProps/core.xml
  created        2026-03-12
  last saved by  Sachin Sarwa
  application    Microsoft Excel
  company        —
  downloaded     —

STRUCTURE
  4 sheets · 12 tables · 1,840 rows
  Q2 Summary     B4:H42    header row 3   currency · date · text
  Regions        A1:D218   header row 1   text · int · currency
  …                                       → marrow file … --tables

INDEX
  parsed     xlsx v0.1.0            exact provenance
  chunks     48 active
  pending    —
  errors     —
```

| Decision | Reason |
|---|---|
| Section headers are SCREAMING, dim | Scannable without bold noise; works when colour is off |
| Extraction method sits beside every derived fact | Principle 6 — you can always tell parsed from inferred |
| `—` for absent | Principle 5. An empty cell reads as "fine"; `—` reads as "we looked, nothing there" |
| Deep detail behind a flag | The default fits one screen. `--tables`, `--history`, `--chunks` expand |

---

## 6. Answers and citations (M4+)

The differentiator is that every claim is traceable. The rendering must make it *cheap* to check, or nobody checks and the provenance is decorative.

```
$ marrow ask "when does the Acme contract renew?"

31 December 2026, with 60 days notice required.

  [1] contracts/acme-2024.pdf:17          §7.2 Renewal        exact
  [2] notes/acme-call.md:34               "…confirmed Dec 31" exact

  Evidence  2 direct, 0 inferred
  Model     local · qwen2.5:7b-q4
  Time      1.2 s   retrieval 40 ms

  → marrow open 1        jump to the source
```

| Rule |
|---|
| **The answer comes first.** Citations follow. Nobody reads a preamble |
| Citations are **numbered and openable** by that number |
| `Evidence  2 direct, 0 inferred` — a claim resting on inference says so |
| **The execution boundary is always shown** (local/private/cloud, UX-012). Never a silent cloud round-trip |
| Contradictions render **both**, with validity, and say so — never silently pick one (§11.3) |
| Below the coverage threshold: `Not found in your files.` and stop. A hedge is worse than an abstention |
| `~approx` badge on any `Degraded`/`Approximate` citation |
| Content from `origin = SELF` renders as `[self]` and **cannot be a citation** (the `origin = SELF` rule) |

---

## 7. Status

```
$ marrow status

melp                              ~/Desktop/melp             live
  9,435 files · 1.0 GB · 48,201 chunks
  indexed 3 min ago · reconciled 2 h ago

pictures                          ~/Pictures                 live
  3,478 files · 416 MB · metadata only
  indexed 3 min ago · reconciled 2 h ago

  2 workspaces · 12,913 files · 1.4 GB · db 42 MB
  queue idle
```

Degraded state is loud, and always carries the fix:

```
melp                              ~/Desktop/melp        poll-only ⚠
  watcher unavailable — inotify/FSEvents limit reached
  falling back to a 5-minute reconciliation sweep
  → marrow doctor watcher

  17 files failed to parse                → marrow job list --failed
  412 cloud-only files not indexed        → marrow workspace hydrate
```

**`poll-only` is never silent** (WATCH-009). A watcher that quietly stopped working produces an index that is quietly wrong, which is worse than one that is loudly broken.

---

## 8. Errors

Shape: **what happened · what it means · what to do.**

```
$ marrow workspace add ~/Library/Mobile\ Documents

✗ cloud-only storage
  FS_PLACEHOLDER_SKIPPED

  ~/Library/Mobile Documents is iCloud Drive. 412 of 508 files are
  placeholders — reading them would download 8.2 GB.

  Marrow will index metadata and skip file contents.

  → marrow workspace add ~/Library/Mobile\ Documents --metadata-only
  → marrow workspace add ~/Library/Mobile\ Documents --hydrate
     downloads 8.2 GB before indexing
```

| Rule |
|---|
| Human-readable headline first, **code second** — the code is for grep and for support, not for the reader's first glance |
| State the consequence in units the user feels: **GB, minutes, file counts** — not "an error occurred" |
| Every error ends in a runnable command |
| Errors go to **stderr**; exit code is non-zero |
| A per-file failure never aborts the run (FS-011) — it lands in `marrow job list --failed` |

### Exit codes

| Code | Meaning |
|---|---|
| 0 | Success. **Also zero results** — an empty search is not an error |
| 1 | Usage error |
| 2 | Not found (a named workspace or file that doesn't exist) |
| 3 | Policy denial — never retried, never escalated |
| 4 | Index unavailable or corrupt; needs `marrow doctor` |
| 5 | Interrupted |
| 70 | Internal invariant violated — a bug here, not the user's fault |

---

## 9. Machine output

```
$ marrow search "auth refresh" --json
{
  "schema": "marrow.search/1",
  "query": "auth refresh",
  "elapsed_ms": 8,
  "branches": ["lexical", "path"],
  "total": 17,
  "results": [
    {
      "rank": 1,
      "score": 0.94,
      "reasons": ["exact"],
      "file_id": "01M17KJXJ2K51K4824XQ56H2Q7",
      "path": "src/auth/token.rs",
      "span": { "kind": "lines", "start": 142, "end": 144 },
      "provenance": "exact",
      "origin": "user",
      "breadcrumb": ["impl TokenService", "fn refresh_token"],
      "preview": "pub async fn refresh_token(&self, ctx: &Ctx) …",
      "modified_ms": 1756339200000
    }
  ]
}
```

| Rule |
|---|
| `schema` is versioned and **stable**. Adding a field is fine; changing a meaning is a new version |
| `file_id`, not just a path — paths move, and a machine consumer must survive that (path is never identity) |
| `span` uses the same `SourceSpan` shape as the core domain type. One serialization, everywhere |
| `provenance` and `origin` are **always present**, never omitted when convenient — an agent must be able to refuse to cite `self` content without special-casing |
| Errors in `--json` mode emit a JSON error object on **stderr**, still non-zero exit |
| **MCP tool results reuse this exact shape.** MCP is a transport, not a second format |

---

## 10. Terminal behaviour

| Concern | Behaviour |
|---|---|
| Colour | Auto-off when not a TTY. Honours `NO_COLOR` and `--no-color`. Colour is never the only carrier of meaning (A11Y-003) |
| Width | Adapts to `$COLUMNS`; truncates the **middle** of paths (`src/…/token.rs`) since both ends carry meaning |
| Piping | Detects non-TTY: no re-render, no progress, plain output |
| Progress | Only for work over ~500 ms, and on **stderr** |
| Interrupt | `Ctrl-C` cancels within 500 ms, leaves the index consistent, exits 5 |
| Pager | Never automatic. It breaks streaming and hides the footer |
| Unicode | Box-drawing and `›` degrade to ASCII when the locale isn't UTF-8 |
| Emoji | None. `⚠` and `✗` only, and only where they carry state |

---

## 11. Deliberately not built

| Not building | Why |
|---|---|
| TUI dashboard | `status` is a snapshot, not something to watch. A TUI is a UI project wearing a terminal costume |
| Interactive fuzzy picker | `fzf` exists and is better. `marrow search --json \| fzf` composes |
| Shell-completion for query text | Completing arbitrary content is noise. Completing *subcommands and workspace names* is worth it and cheap |
| Progress bars for indexing | The full corpus indexes in ~2.4 s (M0). A progress bar would flash |
| Config wizard | Two commands and a config file. A wizard is scope for a user who doesn't exist |
| Notifications | It's a CLI |

---

## 12. What "production grade" means here

Not "has many features". These, specifically:

- [ ] First lexical result renders in **< 50 ms**, always, regardless of semantic state
- [ ] Every result is jumpable with one command, and the path format is one an editor linkifies
- [ ] Every failure names a cause and a runnable action
- [ ] `--json` on every command, schema-versioned, identical data to the human view
- [ ] Degraded state — watcher, provenance, cloud-only — is **impossible to miss**
- [ ] `Ctrl-C` is instant and leaves the index consistent
- [ ] Piping works; stdout is never polluted
- [ ] Colour-blind and `NO_COLOR` safe; no meaning carried by colour alone
- [ ] Zero results explains itself
- [ ] No output that only says "done"
