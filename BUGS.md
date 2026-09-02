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
| ~~B5~~ | **fixed** `11fa855` + `42d773a` · **Answers truncate because retrieval eats the whole context window.** *Root cause found and quantified.* `default_context` is a flat **8,192** tokens. The screenshot's own header says **41 sources, 29 KB of context** — about 7,424 prompt tokens. `answer_budget` computes `8192 − 7424 = 768`, then `.clamp(1024, 4096)` raises it back to the floor, so the model is asked for 1,024 tokens on a window that is already nearly full: about **4,096 characters**, when the request was to generate an HTML page. It *must* stop mid-output. The floor hides the overrun instead of reporting it. The fix is to bound the evidence so the prompt cannot consume the window, and to record what was dropped in the existing `excluded` list (which already carries a reason per source) rather than dropping it silently. Raising `default_context` is not the fix on its own: KV is 160 KB/token, so 8k→16k costs another 1.3 GB on a 16 GB machine. Verified clean on the small corpus: `a_long_answer_is_not_silently_cut_off` finishes at 414 tokens with `stop = stop`; the small corpus never fills the window, which is why the test never caught it. | `desktop/src/ask.rs` (`assemble`), `models.rs` (`answer_budget`) | user report ×2; arithmetic above |
| B6 | **UI wastes space and does not behave like a chat product.** ~230px permanent nav rail for five items; the Ask empty state is three lines centred in a large void with the composer pinned to the bottom; Fast/Thorough are two large cards instead of one compact control. Study Gemini, ChatGPT and Claude. | `App.tsx`, `Sidebar`, `AskView.tsx` | user report + screenshot |
| B7 | **One session only.** No conversation list, no history, no way to return to an earlier thread. Every chat product has this and it is what the sidebar should hold. | `AskView.tsx` | user report |
| ~~B8~~ | **fixed** `ffe2c26` · **`marrow watch` and the desktop both index; neither shares a lock.** Two processes sweeping the same root do duplicate work. Not observed to corrupt anything — the writer is a single actor — but it is wasted and unmeasured. | `cli/src/watching.rs`, `desktop/src/watching.rs` | reasoning, not observed |

---

## Fixed today

| What | Commit |
|---|---|
| **No released build could answer a question on any machine but the one that built it.** The app shipped `mlx_worker.py` and never the interpreter; the printed fix began with a command macOS does not have | *this change* |
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
| ~~C1~~ | **fixed** `3499205` · **critical** | MCP `fetch_url`: "a first fetch of a new host needs the user's confirmation" | It can **never** fetch anything. `Consent::new()` is built fresh inside the handler and never populated, so every URL returns `NewHost` and MCP turns every confirmation into an error. 100% refusal, every host, forever. The description promises a flow that cannot exist. | `mcp/src/server.rs:835`, `net/src/policy.rs:428` |
| ~~C2~~ | **fixed** `74659c1` (CLI); desktop Search view and MCP `search` still lexical-only · **critical** | `marrow embed` builds semantic search; TRACKER says M4 Semantic **Done** | The vector index is read by **one** call site in the repo — desktop **Ask**. `marrow search`, the desktop **Search** view and MCP `search` never open `SqliteVectorIndex`. A 2¼-hour backfill changes nothing on two of the three surfaces. Adding `marrow embed` and the Build button fixed the *producer*; the *consumer* was never wired. | `cli/src/search.rs:48`, `desktop/src/state.rs:369`, `mcp/src/server.rs:147` |
| ~~C3~~ | **fixed** `c456fbb` · high | Error: "Run `marrow reindex`, or delete the index directory" | `marrow reindex` does not exist (it is `index`), and there is no index directory — FTS5 lives inside `marrow.db` (D3), so the fallback tells the user to delete their **corrections**, which hard rule 8 says are the one thing that cannot be rebuilt. | `index/src/fts5.rs:566,654` |
| ~~C4~~ | **fixed** `c456fbb` · high | `--literal` was removed from library messages in `5aff922` | Two survivors in the same file: the too-many-terms error and the FTS5-syntax error both still name a CLI flag on all three surfaces. | `index/src/fts5.rs:388,665` |
| C5 | high | Zero-results page, first run: **"Add a folder"** and **"Run an index"** | Both call `unavailable("policy")` — a message about `workspace_set_policy`, a different feature. Both capabilities now exist and work from the Status page. | `ZeroResults.tsx:80,94` |
| C6 | high | Zero-results: "Nothing is missing from the index — every file in every workspace was read." | Shown whenever three narrow checks pass, which is the ordinary state. It is false: most files have no chunks. `unindexed` arrives on the same query and the component never reads it. | `ZeroResults.tsx:161` |
| C7 | high | Settings: "the **eight read-only commands** are the entire surface between it and the disk" | There are 22 and several mutate — `add_workspace` writes the DB, `download_model` writes 200 MB over the network, `reindex` rewrites the index. The test cited as enforcement only asserts the count and that no *name* contains "write"/"delete". Body copy a user reads to understand the security posture. | `SettingsView.tsx:37,150`, `api.ts:13`, `commands.rs:469` |
| C8 | high | Settings: "cannot start an index run (`index_run`)" | `reindex` exists and Status calls it. | `SettingsView.tsx:46` |
| ~~C9~~ | **fixed** `74659c1` · high | `marrow status` — "Index health" | Prints counts with **no freshness at all**, human or `--json`. The exact bug fixed for MCP and desktop in `dbf4636`; the CLI reads the same `IndexStats` and ignores the three fields. | `cli/src/main.rs:659` |
| ~~C10~~ | **fixed** partly — `search_hybrid` has a caller now; `intelligence` and `explain` still do not · high | TRACKER ticks `marrow-query`: "RRF fusion, FI read model, explain" | `search_hybrid`, `intelligence` (1,049 lines) and `explain` (259 lines) have no caller. The desktop re-implements a smaller fusion inline. ~1,900 lines exercised only by their own tests. | `query/src/{search,intelligence,explain}.rs` |
| C11 | med | MCP `search` "returns ranked excerpts" | Does not pin the snippet column, so FTS5 picks the best-matching column and an excerpt can be the **filename**. The port's own doc says to pin Body when the snippet goes to a model; the desktop does, MCP does not. | `mcp/src/server.rs:144`, `index/src/port.rs:178` |
| ~~C12~~ | **fixed** `e8d72f0` · med | MCP `search` — an agent's default retrieval tool | Defaults to `Terms` (every word must appear), so "when does the lease renew" finds nothing against a document saying "renews". `Any` exists, the desktop uses it, MCP exposes no mode and never warns. | `mcp/src/server.rs:144` |
| ~~C13~~ | **fixed** `e8d72f0` · med | The schema test: "a parameter declared but ignored is the worst kind of bug" | All four schema tests iterate `TOOLS`, not `all()` — the four **write** tools are unchecked. `create_page.title`/`workspace` ship with no description. | `mcp/src/tools.rs:349` |
| ~~C14~~ | **fixed** `74659c1` · med | `--json` "on every command" | `marrow embed --json` and `marrow watch --json` parse the flag and discard it. | `cli/src/main.rs:282,284` |
| C15 | med | `releaseModel` — free the loaded model | Registered, exported, typed — **no caller**. Same shape as `start_semantic_backfill` before it was fixed. | `commands.rs:800`, `api.ts:622` |
| C16 | med | `ask(scope)` | Threaded through five layers with no control that sets it. *(Picker now being added.)* | `commands.rs:756` |
| ~~C17~~ | **fixed** `e8d72f0` · med | MCP `list_workspaces`: "with file counts **and index freshness**" | The payload has no freshness field of any kind. | `mcp/src/tools.rs:178` |
| C18 | med | `SearchHit.reason`: "`exact` \| `semantic` \| `path` \| `recent`", rendered as a badge in three components | Hard-coded to `"exact"` for every hit. The badge is always the same word. | `commands.rs:399` |
| ~~C19~~ | **fixed** `74659c1` · low | Exit codes "a script needs to tell the two apart" | `MODEL_UNAVAILABLE = 5` and `INTERRUPTED = 5` are the same number. | `cli/src/main.rs:32,44` |
| ~~C20~~ | **fixed** `74659c1` · low | Zero-results hint prints `marrow search --literal {query}` | Unquoted, in the one message whose whole purpose is patterns a shell eats. The sibling error 200 lines up quotes it and explains why. | `cli/src/search.rs:243` |
| ~~C21~~ | **fixed** `c456fbb` · **doc** | `CLAUDE.md` — the file every agent is told to obey | "Currently specification-only. No code yet." (15 crates, a shipped app) and "Don't build a UI (D42)" — D42 was reversed. | `CLAUDE.md:9,48` |
| ~~C22~~ | **fixed** `c456fbb` · **doc** | `README.md` | "M1 complete, currently at M2"; "Full text **Tantivy**" (D3 settled FTS5); "Parsers **PDFium**" (D54 settled PDFKit); "UI deferred past M6". | `README.md:5,79,81,84` |
| ~~C23~~ | **fixed** `c456fbb` · **doc** | TRACKER Progress: M4 Semantic **Done** | 11 of its 13 items are unticked, and per C2 it is not true in substance either. | `TRACKER.md:19` |

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


---

## Hard-rule audit (2026-08-30) — markers written out of step with the work

One theme, four instances: **a completion marker is committed before, or
independently of, the work it attests to.** The first is corpus-corrupting and
silent.

| # | sev | What happens | Where |
|---|---|---|---|
| ~~R7-A~~ | **fixed** `c4095ca` · **critical** | `record_version` is sent in its **own** writer closure; `replace_chunks` + `upsert_docs` go in a different one ~100 lines later. The writer batches, so these are separate transactions. A `kill -9` between them — which CLAUDE.md says happens "constantly during development" — leaves a `file_versions` row whose `content_hash` matches the disk and which has **zero chunks**. On the next run the gate is `content_hash != new` → false → the content stage never runs. **The file is permanently unsearchable and nothing will ever notice.** Silent and accumulating. | `ingest/src/pipeline.rs:728` vs `:834` |
| ~~R7-B~~ | **fixed** `db0a9d3` · high | Freshness is stamped when no reconciliation happened. Three callers: before the first sweep (`desktop/src/watching.rs:311` calls `set_health` at `:453`, the sweep is at `:328` — open the app, kill it two seconds later, the row says `LIVE / just now` having walked nothing); on a **cancelled** sweep, where `o.cancelled` is never inspected; and on a health *downgrade*, so freshness improves at the moment coverage degrades. This is the `watcher_health` bug one layer up — a writer now exists and writes the wrong thing. The correct guard already exists at `cli/src/main.rs:533`. | `desktop/src/watching.rs`, `cli/src/watching.rs:202,227,255` |
| ~~R10-A~~ | **fixed** `3245dcc` · high | `marrow search --literal` builds its whole scan scope from the `files` table — the index it exists to bypass. Add a folder, search before any index run: zero targets, the loop body never executes, `stopped` stays `Completed`, every skip counter is 0. The user is told **"0 matches in 0 of 0 files"** with no incompleteness warning: a complete search of a folder nothing opened. Same for any file created since the last sweep — precisely what this command is recommended for. | `cli/src/literal.rs:162` |
| ~~R7-C~~ | **fixed** `093d375` · med | `parser_version` is written and never compared. `content.rs:31` states PAR-003 makes it "the mechanism by which an upgrade schedules reprocessing"; it is read back only for display. `changed` is content-hash only, so a parser fix is dead on arrival for the entire existing corpus. | `ingest/src/content.rs:31` |
| — | low | `download.rs:150` uses `.expect()` outside tests and `main`, against the conventions. | `model/src/download.rs:150` |

Two findings in the model crate (corrupt-install detection, a breaker never
persisted) were relayed without supporting detail and are **not verified**. They
are not on this list until someone looks.


---

## Real-corpus hunt (2026-08-30) — half the index described a disk that had moved on

Run against the author's real 79,186-file index, every answer checked against
SQLite and disk. **F1 and F2 are the same fix and are now done** (`fts5.rs`
joins the canonical tables); the rest are open.

| # | sev | What happens | Evidence |
|---|---|---|---|
| ~~F1~~ | ~~critical~~ | **FIXED.** Search answered from superseded versions and cited a line that says the opposite. 32,436 of 131,519 index docs belonged to HISTORICAL versions; `chunks.status` has a `SUPERSEDED` value **no code has ever written** (0 rows). "What milestone am I on" answered two milestones out of date, citing the file that says otherwise. Of 1,806 historical chunks checked against disk, **917 cited lines that no longer contain their text.** | `SELECT count(*) FROM text_index_docs d JOIN file_versions v USING(version_id) WHERE v.status='HISTORICAL'` |
| ~~F2~~ | ~~critical~~ | **FIXED.** Search returned files the same product says are not indexed. 34,069 docs on `files.status='DELETED'` across 4,724 files. `search` cited a path that `read_file` refuses to open and `file_info` denies knowing. 12 of 248 audited results pointed at paths not on disk. | verified: a sampled deleted path does not exist |
| ~~F3~~ | **fixed** `c456fbb` · **high** | **Only 43% of excerpts are the file's content at the cited line.** `snippet()` is called with column `-1` ("best column") over `path`/`title`/`body`, so when the query matches the filename or the heading, the excerpt *is* the path or the heading — presented as file content. Of 215 checkable results: 93 genuine, 18 the absolute path, 55 the breadcrumb, 14 real text at a different line. The desktop pins Body for exactly this reason; the shared default does not. | `fts5.rs:583` |
| ~~F4~~ | med | **Half fixed, half deliberately not.** The exact duplicates were multi-version rows and went with F1/F2 — measured after: 0 exact duplicates across three queries that previously had them. What remains is the chunker's overlapping granularities: `kv.rs` returns lines 418–434 *and* 356–435. **I tried collapsing on span containment and reverted it.** Two existing tests failed because distinct chunks can legitimately share a byte range, and a rule that drops real evidence to tidy a result list is a worse bug than the one it fixes. The cost is a wasted slot and duplicate text in the evidence budget, not a wrong answer. If it is worth fixing, the safe version keys on the *text* being a substring, not on the span. | `search "kv cache eviction"` |
| ~~F5~~ | **fixed** `3245dcc` · **high** | `marrow search --literal` reports "no matches" for strings that exist, **nondeterministically** — it depends on the OS page cache. Cold: 0 matches, 7,713 files scanned, TimeBudget. Warm: 5 matches. And the advice it prints ("narrow it with a workspace or a path") names flags the CLI does not have — copied from the MCP tool, which does. `--json` omits the incompleteness entirely. | 8 consecutive runs, 2 cold both returned 0 |
| ~~F6~~ | **fixed** `d8f8e24` · **high** | Three surfaces report 35,361 / 79,186 / 766,976 files for the same folder. 43,685 ACTIVE files live inside `target/` and `.git/`, indexed by an earlier build; the walker now prunes those directories, so reconciliation **can never see them** and they are never marked deleted. This is why `.git/config` outranks the actual docs for "admission control". 63% of the `unindexed` count is in pruned directories and can never become indexed by any action the user takes. | before/after a full `marrow index`: 43,685 unchanged |
| ~~F7~~ | **fixed** `c456fbb` · med | `content_bytes` over-reports by 4.02 GB (29%) on every surface. The file count filters `status='ACTIVE'`; the byte sum on the same line does not. Three surfaces agree with each other and all disagree with the database. | `cli/src/main.rs:665`, `query/src/catalog.rs:131` |
| ~~F8~~ | **fixed** `e8d72f0` · med | `marrow status`, described as "Index health", reports no health. And `index_status` returns `files_indexed: 79186, searchable_chunks: 131519`, which reads as "all of them are searchable" — only **21,268 files have any chunk at all**. Its own description promises a skipped count it does not return; `list_workspaces` promises freshness it does not return. | |
| ~~F9~~ | **fixed** `e8d72f0` · med | `file_info` reports deleted files as `citable: true, indexed_for_search: true, tier_state: resident`, while `read_file` on the same path says the file no longer exists. An agent asking whether a source is trustworthy is told yes. | |
| ~~F10~~ | **fixed** `5d6e155` · med | Semantic search covers **239 of 79,186 files** and no surface says so. Any semantic branch answers silently from 0.3% of the corpus. | `chunk_embeddings` = 2,304 rows |
| — | low | `search` never says the result set was cut (`total` always equals what was returned) · MCP `limit` bounds advertised but silently clamped · `read_file` past EOF returns empty rather than saying where the file ends · `tier_state` stays `RESIDENT` for 11k files that no longer exist. | |

### Clean

FTS injection and hostile input (`*`, `"`, `NEAR()`, unterminated quotes — all
escaped, no panics) · unicode and CJK · long/many-term queries bounded and
honest · regex with no ReDoS · `read_file` path traversal refused · findability
by name (IDX-001) holds including for files with no parser · exit codes · no
pathological latency · **no panics, hangs or backtraces in any invocation**.


---

## Fixed since the audits (2026-08-30, later)

| What | Commit |
|---|---|
| Half the index described a disk that had moved on: 32,436 docs from superseded versions, 34,069 from deleted files, all returned as current fact with citations | `753fca7` |
| The version row was treated as proof the file was processed, so a kill mid-run made files permanently unsearchable | `c4095ca` |
| Freshness was stamped when no reconciliation happened — before the first sweep, on a cancelled sweep, and on a health *downgrade* | `db0a9d3` |
| Excerpts were often the file's path or its heading, presented as its content — only 43% were genuine | `c456fbb` |
| `content_bytes` over-reported by 4.02 GB because the byte sum did not filter what the file count filtered | `c456fbb` |
| `fetch_url` refused every URL on every host forever while advertising a confirmation flow | `3499205` |
| Four documents stated things that were not true, including CLAUDE.md's "no code yet" | `c456fbb` |
| `marrow search` ignored semantic search; `marrow status` reported no freshness; `--json` discarded by two commands; two exit codes identical | `74659c1` |
| A generated page opened in a 340px letterbox instead of a side panel | `48d7983` |
| Six library messages named CLI flags that mean nothing on two of three surfaces | `c456fbb`, `5aff922` |

### Judgement call worth knowing about

`marrow search --semantic` is **opt-in**, not the default. Starting the embedder
takes a 40 ms search to 4.7 s, and on this corpus only 239 of 79,186 files have
vectors — so defaulting it on would pay five seconds for a branch that can speak
about 0.3% of the index. The zero-results screen suggests the flag, gated on
vectors existing. Revisit when the backfill has run and an embedder can be kept
resident.


---

## A process note worth keeping

Three times this session a broad `git add -A` swept another agent's in-flight
files into an unrelated commit: the artifact panel into `6413e78`/`07eca57`, CLI
work into `3499205`, and the whole MCP honesty pass into `e8d72f0`, whose message
is about something else entirely. Nothing was lost and the tree is correct each
time, but the history says the wrong thing about where work happened.

The cause is running parallel agents against one working tree and then staging
by wildcard. Stage explicit paths, or give each agent its own worktree.


| Fixed | Commit |
|---|---|
| `marrow search --literal` was scoped by the index it exists to bypass, and returned "0 of 0 files" as a *complete* search; also nondeterministic on the page cache, and its advice named three flags that did not exist | `3245dcc` |
| `read_bounded` stat'd with `metadata`, which follows symlinks — a path that became a link between listing and reading was followed out of the authorised root | `3245dcc` |
| A parser upgrade never reprocessed anything already indexed; `parser_version` was written with every result and only ever read back for display | `093d375` |
| MCP: `limit` bounds advertised and silently clamped; `total` never said a page was cut; `read_file` past EOF returned empty rather than saying where the file ends | `b41b69c` |
| Semantic coverage (1.3% of chunks) invisible on every surface | `5d6e155` |


---

## Missing features, assessed (2026-08-30)

Asked for as essential. Three of the six already existed and had simply never
been seen, because the app had not been rebuilt since they landed — which is
itself worth noting: **shipping and being visible are different events**.

| | State |
|---|---|
| Workspace setup | **exists** — `add_workspace`, native picker in Rust, canonicalizes and refuses overlapping roots |
| Model download and configuration | **exists** — download, cancel, profile, detection, release, with fit arithmetic on the Models page |
| New / clean session, chat history | **exists** as of `226e211` — persists across quit, sidebar list, rename, soft delete, ⌘N |
| File upload | **built** `d77b92e` — drop onto the window or ⌘O; copied into a scratch root, indexed synchronously, answerable in the next breath |
| Images | **built** `c961b95` — Vision OCR, every line with a real bbox `source_span` (D60) |
| Scratch / session workspace | **built** `d77b92e` — a granted, watched root Marrow owns; capped, oldest-first eviction, visible and emptyable from Settings |
| First-run setup | **built** `d77b92e` — grant, watch it index, then the optional model; decided from real state, skippable, re-enterable |
| Video and audio | **scoped, not built** — see the TRACKER parking lot. A shallow version would read as support and answer nothing |

### And one found while checking

**The AI profile never persists.** *(fixed `d77b92e` — a preferences file, not a migration. Two more were found the same way: Fast/Thorough and the sidebar collapse were both re-chosen on every launch.)* `Hub` holds it in a `Mutex<Profile>`
initialised from `default_profile(&machine)` at startup; `set_profile` writes
only to that mutex. Choosing Efficient or LargerLocal survives until the next
launch and then silently reverts to the hardware default — a setting that
appears to work and undoes itself is worse than one that is not offered.


---

## Verified after the big index changes (2026-08-30)

Reconciliation marked 53,814 files DELETED on the author's real index. Checked
that nothing real was caught: of a 400-path sample, **240 still exist on disk
and every one of them is inside a pruned or hidden directory** (`target/`,
`.git/`, `node_modules/`, and so on). Zero real files were dropped.

### But this exposes a gap worth naming

**A file inside a pruned directory can no longer be indexed at all, and there is
no per-root way to say otherwise.** The walk now defines the scope, which is
what makes deletion detection possible — but `DEFAULT_NOISE_DIRS` is a constant,
and D47 resolved that `.gitignore` respect should be *per-root policy*, defaulting
on for roots that look like code. What shipped is a per-**run** `marrow index
--gitignore` flag, off by default, with no column in `workspace_roots` to hold a
per-root choice. So `watch` and the desktop can never honour it either.

The consequence today is small — nobody has asked for a file in `vendor/` — but
it is a real limit stated nowhere in the product: a folder the user genuinely
wants indexed, that happens to be called `dist` or `build`, is silently invisible
and will now also be actively marked deleted. D47's own measurement is the reason
it matters: the noise list removes 62% of files on this corpus, so the exclusions
are doing a great deal of work with no per-root override.

Needs: a `walk_policy` column on `workspace_roots`, a way to see what a root
excludes, and a way to override it. Not urgent; not invisible either.


---

## Open, and why each is still open

Everything reported has been fixed. What remains is three items I chose not to
close, with the reason in each case.

| # | What | Why it is still open |
|---|---|---|
| F4 (part) | The chunker's overlapping granularities put the same passage in a result twice — `kv.rs` as lines 418–434 *and* 356–435 | I built a collapse on span containment and reverted it: two existing tests failed because distinct chunks can legitimately share a byte range, and the rule was one-directional anyway. **Dropping real evidence to tidy a result list is a worse bug than a wasted row.** A safe version keys on the text being a substring, not the span |
| C10 | `query/src/intelligence.rs` — 1,049 lines with no caller | It is a spec-shaped read model built ahead of its consumers. Deleting someone's tested work is not mine to decide, and wiring it wholesale is building ahead of the milestone. TRACKER now says so instead of ticking it |
| — | Per-root walk policy (D47) | The exclusion list is a constant and D47's per-root policy was never built. A folder genuinely wanted that happens to be named `dist` or `build` is invisible *and* now actively marked deleted. Needs a `walk_policy` column, a way to see what a root excludes, and a way to override it |


---

## Found while adding search filters (2026-08-30)

**The semantic branch cannot be filtered.** `marrow_index::VectorQuery` carries a
workspace and nothing else, so the vector branch retrieves without regard to
extension, path or date. The CLI therefore post-filters those hits with
`Filters::admits` — which is deliberately the after-the-fact filtering the
design warns against everywhere else, and the trade was made knowingly: showing
a `.pdf` under `--type html` is a visible falsehood, while losing a deep
semantic candidate is an invisible recall loss.

It is still the wrong shape. Teaching `VectorQuery` to take `Filters` would
push the predicate into the query where the lexical branch already has it, and
`admits` would delete itself.

**The HTML parser extracts no links.** No `IrKind::Link`, no `href` anywhere —
roughly the `TagEnd::Link` handling `markdown.rs` already has. A genuine unmet
requirement, and it belongs in the walker rather than in a competing parser.

### And one non-finding, recorded so it is not re-litigated

**CSS does not need a parser, and that is a decision rather than an omission.**
The harm an HTML parser fixes is that content is buried *inside* markup that is
not content. A stylesheet has no such split: the declarations are the file, so a
parser would re-emit the same bytes under different node kinds. The only prose
is comments — a real 5,957-line stylesheet in this corpus has 145 comment
openers, about 2% — and the text parser already finds them and cites them to an
exact line. No question has been logged that plain text failed to answer for
CSS, and the rule is not to add a parser until a real file demands one.


---

## Adversarial review of the session (2026-08-31)

Nine agents, four independent lenses over ~90 commits, every finding then handed
to an agent whose job was to **refute** it. 12 candidates, **10 survived**. All
four acted on; two were critical and both were in reconciliation code written
the day before.

| # | sev | What | Fixed |
|---|---|---|---|
| A1 | **critical** | A walker error — an unopenable directory, a metadata error, a refused symlink escape — was only `debug!`-logged and bumped on a progress counter that is never read back into the outcome. So the bulk-delete guard, whose own comment says a *failed* run must not conclude anything, saw `failures.is_empty()` and ran. Every file under an unreadable directory was soft-deleted while sitting on the disk, reported as `removed: N, failed: 0` — indistinguishable from a healthy reconciliation of a folder the user emptied. | `2696599` |
| A2 | **critical** | Nothing ever set `files.status` back to `ACTIVE`, so every delete was permanent including a wrong one. A file moved out of a watched folder and back stayed DELETED for ever: the next walk finds it by filesystem identity, restores its path, sees the hash unchanged and reports it *unchanged*, while search and every read tool filter on ACTIVE. No error, no warning, no counter that moves. | `2696599` |
| A3 | high | `chunks.status` has a `SUPERSEDED` value nothing has ever written, so every chunk count included text no search could return — 274,519 counted against 59,197 reachable, a 4.6× over-report on every surface. | `fc3d9bb` |
| A4 | high | `marrow embed` derived its work from the same predicate, offering a two-hour job of which four fifths would embed unreachable text, while `marrow status` suggested running it. The coverage figure divided by the inflated denominator and read 1.26% where the truth is 4%. | `fc3d9bb` |

### What the review did that a single reviewer would not

The refutation stage **improved** A1 rather than merely confirming it: it
narrowed the claim (an unreadable *file* was always caught — hashing records the
failure; the hole is walker errors specifically), killed the reviewer's own
worked example as wrong (an absent mount point yields a successful *empty* walk,
so that case goes through the documented rule rather than this bug), and
supplied a better one that needs no combination to be bad. It also found an
existing test in `walk.rs` proving the mechanism, which neither the reviewer nor
I had connected.

### The structural lesson

Both criticals were unprotected because the existing test covered **one arm of a
two-arm condition**. `a_cancelled_sweep_never_concludes_that_the_files_it_missed_are_gone`
pins the `cancelled` half of `!cancelled && failures.is_empty()` — and reads, to
anyone scanning the list, like coverage of the guard.

---

## Second adversarial review (2026-08-31) — all cleared

Twelve candidates, eleven survived refutation. Every one is now fixed with a
regression test, and each test was mutation-verified: the fix was reverted and
the test confirmed to fail.

| # | sev | What | Fixed |
|---|---|---|---|
| C1 | **critical** | **Grading a two-cell table asked the allocator for 17.2 GB.** `grade` painted every square of a table's bounding box into a `Vec<bool>`. A workbook with a value in A1 and one in XFD1048576 has two cells and a box of 17,179,869,184 squares. It never crashed for two reasons that are both accidents: `vec![false; n]` allocates zeroed so the pages are mapped lazily and never touched, and `all()` short-circuits on the first hole — index 1 in a sparse table. An allocator that refuses aborts the process uncatchably. | `ee6c87a` |
| C2 | **critical** | **The chunker walked the same box.** `table_chunks` rendered `body_start()..n_rows` × `0..n_cols`, so the same workbook produced roughly 51 GB of chunk text, with no ceiling and outside any budget. Measured at 1/2,850th of the box: 6,668 chunks from two cells. The XLSX parser is careful about exactly this and says so in a comment; the chunker undid it one layer up. `render_row` and `schema_text` had the same shape. | `654053f` |
| C3 | high | **A change to the chunker reached no already-indexed file.** `CHUNKER_VERSION` was documented from the start as "persisted so a change can schedule re-chunking" and written faithfully with every chunk — and the staleness gate compared only *parser* versions, so nothing ever read it back. Exactly the stale index the reconciler exists to prevent. | `654053f` |
| C4 | high | **`decode_entities` panicked on ordinary web prose.** It looked for the `;` in a fixed 12-byte window after an `&` and sliced there. `Tom & Jerry café` puts the `é`'s first byte at exactly 12, so the slice split a character — a whole file lost to a stray ampersand. | `0b1caf1` |
| C5 | high | **`<SCRIPT>` bodies were indexed as prose.** `name_at` returns the tag name as written and the RAW set is lowercase, so `RAW.contains(&name)` skipped `<script>` and missed `<SCRIPT>` — which is what a Word "Save as Web Page" export produces. Searching for a function name then matched every page that called it. | `0b1caf1` |
| C6 | high | **Skipping a raw element was quadratic, and the budget could not stop it.** Each skip lowercased the whole remainder of the document, allocating a copy of the tail: 16 s at 4 MB. And `tokenize` ran to completion before anything checked the clock, so the one phase that most needed a deadline had none. The 1 MB regression fixture did not finish in five minutes against the old code; it takes 0.15 s now. | `0b1caf1` |
| C7 | high | **`--explain` denied a branch that ran.** The branch list was derived from whether a *surviving* hit carried a semantic rank. A semantic branch that ran and returned nothing, or whose candidates were outranked past the limit or removed afterwards by a `--type` filter it does not apply itself, vanished — and the caveat then stated flatly that "only one branch ran", on a page whose own footer said otherwise. `search_hybrid` already returns `SearchResults::branches` for exactly this. | `138da1f` |
| C8 | med | **`read_table` ended on half a row.** It clipped at a flat 400 cells, which lands mid-row: a thirty-column table came back with a last row holding ten cells. `truncated` said something had been cut, not that the *last row* was, so an agent reading or summing that row read a row that never existed. | `138da1f` |

### The structural lesson, again

C1 and C2 are the same mistake in two places, and the XLSX parser had already
made the argument against it — in a comment, about work "proportional to an
address the file merely mentioned". A rule learned in one module does not
propagate to its callers by being written down near where it was learned.

C1 is also a reminder that **the platform can hide a bug rather than expose
it.** Peak RSS for the 17.2 GB request is 12 MB, so no memory measurement would
ever have found it. The test counts bytes *requested* through a global
allocator, because the request is the defect and residency is what forgave it.
