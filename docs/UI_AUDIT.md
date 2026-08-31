# UI and UX audit

Four independent passes over the desktop app — shell and navigation, the Ask
surface, configurability, and the design system — with contrast ratios computed
rather than estimated and every claim carrying a `file:line`.

The findings are below. The reason they are worth reading together is that most
of them are the same mistake.

---

## The through-line: the comments are a specification nothing checks

Marrow's source is unusually well commented. That is a real asset and it is also
where this audit kept landing, because a comment that states an invariant reads
exactly like an invariant that holds. Four of these were found in one afternoon:

| The claim | Where | What is true |
|---|---|---|
| "every mouse action is reachable by keyboard" | `App.tsx:4-7`, GUI §11 | Tab is intercepted in five of six views and focuses `null` on Settings — no control on that page can be reached, including the API-key field |
| "a browser or system text-size setting must still be able to scale this. Every size below is derived from it" | `global.css:14-17` | There is no `rem` in any of the 29 stylesheets. Every size is a px literal. System text size is inert |
| "a citation inside a code fence is left alone — a model writing `[E1]` in an example is not making a claim" | `lib/markdown.ts:100-103` | `linkCitations` runs over the whole sanitised document, so `[E1]` inside `<pre><code>` does become a chip |
| stroke weight 1.5 | `Icon.tsx:2` | `strokeWidth={1.8}` at `Icon.tsx:133` |

This is [LESSONS.md](LESSONS.md) §1 — a value written faithfully and never read
back — reappearing one layer up. There, a persisted field described a mechanism
nobody consumed. Here, a comment describes a behaviour nobody enforces. Same
shape, same invisibility, same fix: when the prose asserts something, something
has to fail when it stops being true.

**And the reason none of it was caught: there are no UI tests at all.** No
`.test.tsx`, no `.spec.tsx`, no `test` script in `package.json`, against 7,595
lines of TSX and 5,958 lines of CSS. The Rust side has over 1,100 tests and a
named-invariant list that `check.sh` walks one by one and fails on. The surface
the user actually touches has none, which is why every UI defect this month was
found by the author using the app.

---

## 1. The product's central claim is unreachable on its primary screen

This is the finding. Everything else is a distant second.

Marrow exists to answer with a citation to an exact page, cell or line. On the
Ask view a source is rendered as a non-interactive `<li>` holding the id, a
location string and an excerpt (`AskView.tsx:1234-1244`). `Citation.path` and
`Citation.line` are computed in Rust, carried across the IPC boundary
(`api.ts:755-758`) and **discarded**. `openPath` and `revealPath` exist, work,
and are called from other views (`actions.ts:94,108`) — never from Ask.

Getting from a sentence to its source is therefore: click `[E1]`, read a path,
leave for Search, retype it. GUI §11 asks for one action.

Two compounding defects in the same area:

- **Provenance is invisible.** The sources block is a `<details>` with no `open`
  attribute (`AskView.tsx:1208`), so a finished answer shows a bare count until
  clicked. `Citation.provenance` is computed (`ask.rs:380,446`), crosses the
  boundary, and is rendered nowhere. An answer built from degraded OCR is
  indistinguishable from one built from exact PDF spans — the distinction the
  provenance classes exist to make.
- **The line number is the first thing truncated.** `location` is `path:line`
  (`ask.rs:377`) inside `white-space: nowrap; text-overflow: ellipsis`
  (`AskView.module.css:356-363`). On any real path the ellipsis eats the suffix,
  which is the part that makes it a citation rather than a filename.

---

## 2. Blockers

**Keyboard access is broken across most of the app.** `App.tsx:272-286`
intercepts Tab and calls `cyclePane` in every view except Ask, but `cyclePane`
(`store.ts:325-333`) only has refs attached in Search and partly Files. Models,
Status and Settings get none, so Tab focuses `null`. There are no `⌘1-6`
shortcuts and no command palette — `⌘K` is a five-result file lens
(`QuickFind.tsx`), not the palette GUI §5.1 specifies — so those three views
have no keyboard route at all. `ShortcutsDialog.tsx:31` relabels `⌘K` rather
than marking it absent, which is how the gap stayed quiet.

**A folder that is still indexing is styled as a folder that failed.**
`Sidebar.tsx:57-64` maps `files === 0 && !scratch` to `degraded` with a warning
triangle. The thirty seconds after a new user grants their first folder — the
moment the Welcome flow just told them they had succeeded — looks identical to a
real failure. `WorkspaceRow` (`api.ts:136-160`) carries no queue or job field,
so the UI genuinely cannot tell "not yet" from "never".

**A control that persists a choice and changes nothing.** `set_ai_profile`
writes `preferences.json` (`prefs.rs:50,116`) and survives a restart.
`local_generator` (`models.rs:764-812`) never reads it; `choose(profile, …)` is
consulted only to caption the radio list that sets it (`models.rs:1607`) and to
fill a display table (`models.rs:1805`). Three more controls are visibly inert:
"Retry parsing", "Keep as is" and "Download" all call `unavailable()`
(`StatusView.tsx:252,278,282`), on a page whose neighbour states the opposite
rule (`SettingsView.tsx:9-14`).

**Two bugs found while auditing, neither of them a UI issue.** The Search view
never runs the semantic branch — `state.rs:384,433` hard-code
`branches: ["lexical"]`, and only `retrieve` (`state.rs:257`) fuses — while
`ModelsView.tsx:744` tells the user "Searches match on meaning as well as
words". And `apply_hints` (`pipeline.rs:517-533`) never consults `policy.walk`,
so a file created inside `node_modules`, `.git` or `target` **is** indexed when
the watcher hints it, then pruned by the next sweep.

---

## 3. Annoyances, in the order a daily user meets them

1. **Escape wipes the search query from any view** (`App.tsx:261-271`,
   unguarded), and when the query is already empty it blurs whatever has focus —
   including the Ask composer you are typing in.
2. **The composer never grows.** `rows={1}` (`AskView.tsx:728`) with no
   auto-size and no `field-sizing`. A three-line question scrolls its own first
   line away, and Shift+Enter is documented nowhere.
3. **Scroll-follow still loses to the keyboard.** The recent fix bound `release`
   to `onWheel`/`onTouchMove` only (`AskView.tsx:692-693`); Page Up, arrows,
   space and scrollbar drags emit neither, so the layout effect yanks you back
   on the next token. An incomplete fix, not a new bug.
4. **The wait clock resets per stage** (`AskView.tsx:1081-1085`), so a
   fifty-second first answer reads as three short waits. The one explanatory
   note is gated on `stage === "loading"`, so a slow retrieval explains nothing.
5. **A failed turn is a dead end.** Copy and Retry are gated on `turn.usage`
   (`AskView.tsx:1306`) and Rust emits `Failed` *or* `Done`, never both
   (`ask.rs:552,560,599`). Only `MOD_NOT_INSTALLED` offers an action.
6. **Error, truncation and "3 sources not used" share one colour.**
   `--tint-warn-soft`/`--warn-text` for all three; `--error` is unused in this
   view.
7. **View state half-survives.** Files keeps `workspaceFilter` (store) and loses
   `filter` and `selected` (component state, `FilesView.tsx:38-39`). Search
   loses scroll but keeps the anchor, so returning to result 200 shows the top
   of the list while `↵` still opens the invisible anchored row — the
   open-the-wrong-file failure GUI §5.2 exists to prevent, arriving by another
   door.
8. **Six views as peers, three of which are settings screens.** They already
   overlap: Settings configures a model endpoint beside a whole Models view, and
   carries an "Index" section beside a whole Status view. B6's own design note
   called these "settings-shaped" and they shipped as peers.

---

## 4. Design system: strong, with two precise failures

**Undefined tokens — the bug class that keeps recurring.** `--raised`
(`AskView.module.css:595`, `StatusView.module.css:311`) and `--r-md`
(`StatusView.module.css:309`) are defined nowhere, so the Ask scope `<select>`
and the Add-workspace button render with no background, and the Add button is
the only square control in a 6px-radius app. An undefined custom property
computes to nothing, which is why this is invisible rather than loud. It is the
third instance this month.

**Contrast fails in light, passes in dark.** Every text pair in dark clears
4.5:1. Light has five failures, all 11px pill labels on a 12–14% tint of their
own tone — `--ok` on `--tint-ok` is **3.75:1** on sheet and **3.31:1** on
sunken. The cause is visible in the tokens: the dark composites were measured
and the tones lifted a step (see the note at `tokens.css:182-190`); the same
measurement was never re-run for light. Separately `--fg4` is used as real text
at `ModelsView.module.css:457` — **2.78:1 in dark** — against its own token
contract, which states that nothing sets text in `--fg4`.

**The type scale does not hold.** Nine steps with seven between 9.5px and 13px,
so half-pixel gaps cannot read as hierarchy. Usage is bottom-heavy: `--fs-xs`
(10.5px) has 61 uses against `--fs-row` (13px, the declared base) at 7. The
app's centre of gravity is 10.5px. Long-form answers render at 12.5px, and
citation chips — the differentiator — are 0.85em of that, about 10.6px.

**What is already good, and it is a lot.** Dark mode is the best-built part of
the system: one palette keyed on `data-theme` with no media-query duplicate that
could drift, and every theme-sensitive literal in a component shipping an
explicit dark pair. Motion is fully honoured — all eight `@keyframes` are gated
behind `prefers-reduced-motion`, with one leak at `StatusView.module.css:315`.
Token discipline is real: three raw hex values in ~6,000 lines, each commented
and theme-paired. Focus has one global indicator with its reasoning recorded and
exactly one unreplaced `outline: none`. Responsiveness closes arithmetically at
the 720×480 minimum, and the artifact panel's overlay threshold is stated
identically in TypeScript and CSS.

---

## 5. What "make it configurable" should mean

Configurability is a maintenance cost and a way for a user to break their own
install, so the answer is deliberately short. **Four numbers, one switch, and a
bug fix.**

Surface on the existing Settings page, all already in `preferences.json` shape:

| Setting | Today | Where |
|---|---|---|
| Answer ceiling | fixed 4096 / floor 1024 | `models.rs:1687,1690` |
| Evidence chunks | fixed 12 | `ask.rs:37` |
| Evidence bytes | fixed 16 KB | `ask.rs:51` |
| Thinking budget (what Thorough *is*) | fixed 4096, and nothing else changes | `request.rs:26` |
| Model idle-unload | 180 s, with a 120–300 clamp already present | `supervisor.rs:43` |
| `respect_gitignore`, per workspace | CLI-only flag; D47's second box genuinely unticked | `walk.rs:65,86` |

**Before any of that, fix the AI preference.** It is the most expensive control
in the app precisely because it works — it persists a choice across restarts and
changes nothing.

**Interface size is not a setting, it is an accessibility defect.** The ramp is
px on `:root { font-size: 16px }` with no `rem`, so OS text size and browser
zoom are both inert. The fix is the ramp in `rem` plus one `--ui-scale`, and it
touches every module stylesheet.

### What must stay a decision, not a preference

Placeholder hydration (rule 3 — the inert "Download" button should stay inert),
`follow_links` and the root-overlap refusal (rules 2 and 5 — a switch here is an
exfiltration surface), the `origin = SELF` exclusion (rule 9 — "let it cite its
own notes" is the loop the rule breaks), retrieved text entering the system
prompt (rule 4), search's independence from model, GPU and network (rule 10 —
semantic stays additive, never a semantic-only mode), key storage location
(LLM-030), the parse and hash byte caps and the zip-bomb guards (raising them
buys a killed process, not a better answer), and chunk sizes (changing them
invalidates the index; if they move, they move with a reindex, not a slider).

There should also be no second "advanced settings" surface.
`preferences.json` is hand-editable by design and preserves unknown keys
(`prefs.rs:22-25,248-262`). That is already the escape hatch.

---

## 6. Suggested order

1. **Make a citation open its file**, show provenance, stop truncating the line
   number. This is the product.
2. **Fix the controls that lie** — the AI preference, the three inert Status
   buttons, the semantic-branch claim on the Models page.
3. **Keyboard**: release Tab everywhere, add `⌘1-6` and `⌘,`, guard Escape by
   view. A command palette would subsume most of this.
4. **Indexing is not failing** — give `WorkspaceRow` a queue count and stop
   styling a new folder as degraded.
5. **The two non-UI bugs** — Search's missing semantic branch, and `apply_hints`
   ignoring the walk policy.
6. **Undefined tokens and light-mode contrast.** Mechanical, verifiable, and
   worth a test that fails on the next one.
7. **Configurability** as scoped in §5.
8. **`rem` and `--ui-scale`.** Largest change, least urgent, but it is what makes
   the claim in `global.css` true.

And underneath all of it: **some UI tests**, so the next one of these is caught
by the suite rather than by using the app.
