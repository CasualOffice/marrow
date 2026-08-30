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
| B4 — generated HTML opened on its own markup, scrolled to the middle, and reloaded the frame on every token | `ced9990` |


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


---

## Found by audit, not by use (2026-08-30)

Four read-only sweeps run after the reported bugs were fixed, on the principle
that one bug is rarely one bug. The two criticals were both invisible from
every surface: nothing errors, nothing logs, the feature simply does not happen.

| # | sev | What you are told | What happens | Where |
|---|---|---|---|---|
| C1 | **critical** | MCP `fetch_url`: "a first fetch of a new host needs the user's confirmation" | It can **never** fetch anything. `Consent::new()` is built fresh inside the handler and never populated, so every URL returns `NewHost` and MCP turns every confirmation into an error. 100% refusal, every host, forever. The description promises a flow that cannot exist. | `mcp/src/server.rs:835`, `net/src/policy.rs:428` |
| C2 | **critical** | `marrow embed` builds semantic search; TRACKER says M4 Semantic **Done** | The vector index is read by **one** call site in the repo — desktop **Ask**. `marrow search`, the desktop **Search** view and MCP `search` never open `SqliteVectorIndex`. A 2¼-hour backfill changes nothing on two of the three surfaces. Adding `marrow embed` and the Build button fixed the *producer*; the *consumer* was never wired. | `cli/src/search.rs:48`, `desktop/src/state.rs:369`, `mcp/src/server.rs:147` |
| C3 | high | Error: "Run `marrow reindex`, or delete the index directory" | `marrow reindex` does not exist (it is `index`), and there is no index directory — FTS5 lives inside `marrow.db` (D3), so the fallback tells the user to delete their **corrections**, which hard rule 8 says are the one thing that cannot be rebuilt. | `index/src/fts5.rs:566,654` |
| C4 | high | `--literal` was removed from library messages in `5aff922` | Two survivors in the same file: the too-many-terms error and the FTS5-syntax error both still name a CLI flag on all three surfaces. | `index/src/fts5.rs:388,665` |
| C5 | high | Zero-results page, first run: **"Add a folder"** and **"Run an index"** | Both call `unavailable("policy")` — a message about `workspace_set_policy`, a different feature. Both capabilities now exist and work from the Status page. | `ZeroResults.tsx:80,94` |
| C6 | high | Zero-results: "Nothing is missing from the index — every file in every workspace was read." | Shown whenever three narrow checks pass, which is the ordinary state. It is false: most files have no chunks. `unindexed` arrives on the same query and the component never reads it. | `ZeroResults.tsx:161` |
| C7 | high | Settings: "the **eight read-only commands** are the entire surface between it and the disk" | There are 22 and several mutate — `add_workspace` writes the DB, `download_model` writes 200 MB over the network, `reindex` rewrites the index. The test cited as enforcement only asserts the count and that no *name* contains "write"/"delete". Body copy a user reads to understand the security posture. | `SettingsView.tsx:37,150`, `api.ts:13`, `commands.rs:469` |
| C8 | high | Settings: "cannot start an index run (`index_run`)" | `reindex` exists and Status calls it. | `SettingsView.tsx:46` |
| C9 | high | `marrow status` — "Index health" | Prints counts with **no freshness at all**, human or `--json`. The exact bug fixed for MCP and desktop in `dbf4636`; the CLI reads the same `IndexStats` and ignores the three fields. | `cli/src/main.rs:659` |
| C10 | high | TRACKER ticks `marrow-query`: "RRF fusion, FI read model, explain" | `search_hybrid`, `intelligence` (1,049 lines) and `explain` (259 lines) have no caller. The desktop re-implements a smaller fusion inline. ~1,900 lines exercised only by their own tests. | `query/src/{search,intelligence,explain}.rs` |
| C11 | med | MCP `search` "returns ranked excerpts" | Does not pin the snippet column, so FTS5 picks the best-matching column and an excerpt can be the **filename**. The port's own doc says to pin Body when the snippet goes to a model; the desktop does, MCP does not. | `mcp/src/server.rs:144`, `index/src/port.rs:178` |
| C12 | med | MCP `search` — an agent's default retrieval tool | Defaults to `Terms` (every word must appear), so "when does the lease renew" finds nothing against a document saying "renews". `Any` exists, the desktop uses it, MCP exposes no mode and never warns. | `mcp/src/server.rs:144` |
| C13 | med | The schema test: "a parameter declared but ignored is the worst kind of bug" | All four schema tests iterate `TOOLS`, not `all()` — the four **write** tools are unchecked. `create_page.title`/`workspace` ship with no description. | `mcp/src/tools.rs:349` |
| C14 | med | `--json` "on every command" | `marrow embed --json` and `marrow watch --json` parse the flag and discard it. | `cli/src/main.rs:282,284` |
| C15 | med | `releaseModel` — free the loaded model | Registered, exported, typed — **no caller**. Same shape as `start_semantic_backfill` before it was fixed. | `commands.rs:800`, `api.ts:622` |
| C16 | med | `ask(scope)` | Threaded through five layers with no control that sets it. *(Picker now being added.)* | `commands.rs:756` |
| C17 | med | MCP `list_workspaces`: "with file counts **and index freshness**" | The payload has no freshness field of any kind. | `mcp/src/tools.rs:178` |
| C18 | med | `SearchHit.reason`: "`exact` \| `semantic` \| `path` \| `recent`", rendered as a badge in three components | Hard-coded to `"exact"` for every hit. The badge is always the same word. | `commands.rs:399` |
| C19 | low | Exit codes "a script needs to tell the two apart" | `MODEL_UNAVAILABLE = 5` and `INTERRUPTED = 5` are the same number. | `cli/src/main.rs:32,44` |
| C20 | low | Zero-results hint prints `marrow search --literal {query}` | Unquoted, in the one message whose whole purpose is patterns a shell eats. The sibling error 200 lines up quotes it and explains why. | `cli/src/search.rs:243` |
| C21 | **doc** | `CLAUDE.md` — the file every agent is told to obey | "Currently specification-only. No code yet." (15 crates, a shipped app) and "Don't build a UI (D42)" — D42 was reversed. | `CLAUDE.md:9,48` |
| C22 | **doc** | `README.md` | "M1 complete, currently at M2"; "Full text **Tantivy**" (D3 settled FTS5); "Parsers **PDFium**" (D54 settled PDFKit); "UI deferred past M6". | `README.md:5,79,81,84` |
| C23 | **doc** | TRACKER Progress: M4 Semantic **Done** | 11 of its 13 items are unticked, and per C2 it is not true in substance either. | `TRACKER.md:19` |

### Came back clean

Tauri command registration (all 22 agree across three lists) · every advertised
keyboard shortcut has a live handler, and the one that does not is marked
missing with its reason · MCP **read**-tool parameter handling · every CLI
subcommand does what its help says · `.mcp.json` and `tauri.conf.json` resolve
and match their claims · `search_literal`'s coverage block genuinely reports
what it promises · serde field names across every struct on the Rust↔TS
boundary, both directions · no `flatten`/`untagged`/`skip_serializing_if` on the
boundary, so `Option` arrives as explicit `null` and FI-003 holds · all numerics
well inside 2^53; every timestamp epoch-ms.
