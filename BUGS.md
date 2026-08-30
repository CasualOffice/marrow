# Open bugs and findings

Everything found by **using** Marrow, not by testing it. The suite was green
through every one of these. Ordered by how much they mislead, not by effort.

Fixed items move to the TRACKER Log and come off this list.

---

## Open

| # | What | Where | Evidence |
|---|---|---|---|
| B1 | **The Status page alarms about normal behaviour.** `42,581 unindexed` with a warning triangle. Of 46,129 parse results **zero are FAILED**; 35,422 are `METADATA_ONLY` — photos and binaries with no parser, which the spec calls expected, not a failure. The count conflates "no parser" with "parse failed". | `query/src/catalog.rs`, `StatusView.tsx` | screenshot; `SELECT outcome, count(*) FROM parse_results` |
| B2 | **Dead buttons.** "Retry parsing", "Run an index", "Download", "Keep as is" all call `unavailable()`, which explains that the command does not exist. Retry is also aimed at B1's non-problem. The freshness banner added today points at the same dead `reindex`. | `ui/src/actions.ts`, `StatusView.tsx`, `ZeroResults.tsx` | user report: "retry parsing doesnt work" |
| B3 | **Ask merges unrelated projects.** "whats STT?" returned MFA settings, TaskRanking, Code of Conduct — the workspace is all of `~/Desktop/melp`, which is many services. No way to scope a question to one project, and no signal in the answer that sources span several. | `desktop/src/state.rs` (`retrieve`), `ask.rs` | screenshot: 24 sources across unrelated services |
| B4 | **Generated HTML is shown badly.** The code block opens scrolled to its middle, shows a wall of raw markup, and "Run it" does not say what running it does. Creating a file and previewing it is the flow, and the flow is not there. | `ui/src/components/Answer.tsx` | screenshot |
| B5 | **Answers truncate because retrieval eats the whole context window.** *Root cause found and quantified.* `default_context` is a flat **8,192** tokens. The screenshot's own header says **41 sources, 29 KB of context** — about 7,424 prompt tokens. `answer_budget` computes `8192 − 7424 = 768`, then `.clamp(1024, 4096)` raises it back to the floor, so the model is asked for 1,024 tokens on a window that is already nearly full: about **4,096 characters**, when the request was to generate an HTML page. It *must* stop mid-output. The floor hides the overrun instead of reporting it. The fix is to bound the evidence so the prompt cannot consume the window, and to record what was dropped in the existing `excluded` list (which already carries a reason per source) rather than dropping it silently. Raising `default_context` is not the fix on its own: KV is 160 KB/token, so 8k→16k costs another 1.3 GB on a 16 GB machine. Verified clean on the small corpus: `a_long_answer_is_not_silently_cut_off` finishes at 414 tokens with `stop = stop`; the small corpus never fills the window, which is why the test never caught it. | `desktop/src/ask.rs` (`assemble`), `models.rs` (`answer_budget`) | user report ×2; arithmetic above |
| B6 | **UI wastes space and does not behave like a chat product.** ~230px permanent nav rail for five items; the Ask empty state is three lines centred in a large void with the composer pinned to the bottom; Fast/Thorough are two large cards instead of one compact control. Study Gemini, ChatGPT and Claude. | `App.tsx`, `Sidebar`, `AskView.tsx` | user report + screenshot |
| B7 | **One session only.** No conversation list, no history, no way to return to an earlier thread. Every chat product has this and it is what the sidebar should hold. | `AskView.tsx` | user report |
| B8 | **`marrow watch` and the desktop both index; neither shares a lock.** Two processes sweeping the same root do duplicate work. Not observed to corrupt anything — the writer is a single actor — but it is wasted and unmeasured. | `cli/src/watching.rs`, `desktop/src/watching.rs` | reasoning, not observed |

---

## Fixed today

| What | Commit |
|---|---|
| CLI and MCP could not open the desktop's index at all — migration chain drift | `8706630` |
| Semantic search built, tested, and unreachable: no CLI command, no UI control | `8706630` |
| `search_literal` not exposed over MCP | `5aff922` |
| Library error suggested `--literal`, a CLI flag, on every surface | `5aff922` |
| Index silently nine hours stale; nothing watched; `watcher_health` defaulted to `LIVE` and was never written | `dbf4636` |
| A change made while the app was shut waited six hours for the scheduled sweep | `dbf4636` |
| Every answer footer read `tokens in NaNm NaNs`; the truncation notice never rendered | `e883546` |
| "What model are you using?" answered from the corpus, offering GPT-4 / Llama-3 | `e883546` |
| No text was selectable anywhere | `e883546` |
| No way to add a folder from the app | `cd9b50f` |
| Switching tabs destroyed the Ask conversation and orphaned a running generation | `cd9b50f` |


---

## B6 / B7 — the design, before it is built

Studied against Gemini, ChatGPT and Claude. What all three do that this window
does not, and why each one matters here.

**1. The sidebar holds conversations, not modes.** All three give that column to
*content*: a "New chat" button and a reverse-chronological list of threads.
Ours spends 176px on six app sections (Search / Ask / Files / Models / Status /
Settings) plus a workspace list. That is an IDE shape, not a chat shape. Modes
that are visited once a week do not deserve permanent width; the thing you
return to constantly does.

→ Sidebar becomes: **New conversation**, then the thread list, then workspaces.
The six sections move to a compact switcher — Search and Ask are the two real
surfaces, and Files/Models/Status/Settings are settings-shaped.

**2. The composer is centred when the thread is empty.** Gemini, ChatGPT and
Claude all open with the input in the middle of the screen under a short
greeting, and drop it to the bottom once the first answer exists. Ours pins it
to the bottom from the start, leaving a large void with three lines of grey
text floating in it — which is exactly the wasted space that was reported.

**3. Mode is a compact control inside the composer.** Ours renders Fast and
Thorough as two ~280×56px cards on their own row. All three put the equivalent
(model picker, tools, reasoning toggle) as a small control in or beside the
input. The row should collapse into one segmented control on the composer's
left, beside Ask.

**4. The rail collapses.** All three can hide the sidebar. Ours has the grid
column already (`--shell-nav-w` → 0), so this is exposure, not construction.

**5. Threads persist.** B7. Nothing survives a restart, and there is no way back
to yesterday's question. Storage exists — SQLite, one writer — so this is a
table and a list, not an architecture. It is what makes the sidebar worth its
width, so B6 and B7 ship together.

Not adopted: the three products stream into a full-width column with a model
picker at top-centre. Marrow's answers carry citations and an evidence list,
which those products do not have, and the existing `--prose-measure` column
plus the sources disclosure is the better fit. Copying their chrome wholesale
would cost the thing that makes this different.
