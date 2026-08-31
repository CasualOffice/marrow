# Marrow — Desktop Application Design

**Status:** Design. Precedes implementation of `marrow-desktop`.
**Reverses:** D42 (defer UI past M6) and the UI portion of Part 7 §130. The desktop app is the product surface, not an eventual extra.
**Retains:** Part 7 §130's other half — we still do not build our own agent runtime or model gateway. MCP still exposes the index to Claude Code.
**Companion:** [UX.md](UX.md) (terminal), [LLD.md](LLD.md) (internals).

---

## 1. What changed and what didn't

| | Before | Now |
|---|---|---|
| Primary surface | Terminal + MCP | **Desktop app** |
| CLI | The product | Retained — scripting, debugging, CI. Nearly free once the core exists |
| MCP | The product | Retained — it's how Claude Code reaches the index |
| Agent runtime, model gateway, approval UX | Not built (§130) | **Still not built.** A GUI does not require owning inference |
| Part 1 §16 IA | Superseded | **Back in scope**, trimmed to what ships |

**Three frontends, one core.** This strengthens the ports-and-adapters design in [LLD](LLD.md) rather than complicating it: `desktop`, `cli` and `mcp` are all adapters over `marrow-query`. If a frontend computes something the others can't reach, the boundary is in the wrong place.

---

## 2. Stack

| Layer | Choice | Why |
|---|---|---|
| Shell | **Tauri 2** | Rust core links directly — no IPC serialization tax, no second language for logic. ~10 MB vs Electron's ~150 MB, and the corpus already lives on this machine |
| UI | **React + TypeScript** | Part 1 §17.1. Boring on purpose |
| Server state | **TanStack Query** | Every panel is a query against the local core; caching, invalidation and stale-while-revalidate are exactly its job |
| Client state | **Zustand** | UI-only: selection, pane sizes, active view. **Never mirrors core state** |
| Virtualization | **TanStack Virtual** | Result lists and the file table |
| Styling | **CSS Modules + design tokens** | No runtime CSS-in-JS. This is a native-feeling tool, not a themeable web app |
| Icons | **Lucide** | Consistent stroke weight, tree-shakeable |

**No component library.** Radix primitives for the four things that are genuinely hard (dialog, popover, context menu, focus trap); everything else is ours. A component library would fight the density this app needs.

### The Tauri boundary

```
┌──────────────────────────────────────────┐
│  WebView — React                         │  presentation only
│  no fs access, no network, narrow scope  │
└────────────────┬─────────────────────────┘
                 │  Tauri commands (typed, generated)
┌────────────────▼─────────────────────────┐
│  marrow-desktop (Rust)                   │  thin adapter
│  command handlers · event emitters       │
└────────────────┬─────────────────────────┘
                 │
          marrow-query ──▶ store · index · scan
```

| Rule |
|---|
| The WebView gets **no** filesystem plugin, no shell, no network. Every capability is an explicit named command (SEC-012) |
| Command handlers contain **zero logic** — they deserialize, call `query`, serialize |
| TypeScript types are **generated** from the Rust command signatures. A drift check fails CI |
| Long operations emit **events**, they don't block a command |
| File contents reach the WebView only as **bounded excerpts** — never a whole file, never a directory listing the user didn't ask for |

---

## 3. The two modes

A knowledge tool has two distinct usage shapes, and cramming them into one window serves neither.

### 3.1 Quick find — a global overlay

The 80% case: *"where is that thing"*. Invoked by a global hotkey from anywhere, answers in under 50 ms, disappears.

```
┌───────────────────────────────────────────────────────────┐
│  ⌕  auth refresh token                                    │
├───────────────────────────────────────────────────────────┤
│  ⌘1  token.rs                          exact · 2d         │
│      src/auth/                                            │
│      pub async fn refresh_token(&self, ctx: &Ctx)         │
│                                                           │
│  ⌘2  auth-design.md                 semantic · 3w         │
│      docs/                                                │
│      ## Refresh token rotation                            │
│                                                           │
│  ⌘3  2026-06-standup.md             semantic · 8w         │
│      notes/                                               │
├───────────────────────────────────────────────────────────┤
│  ↵ open   ⇧↵ reveal   ⌘↵ open in Marrow   17 · 8ms        │
└───────────────────────────────────────────────────────────┘
```

| Decision | Reason |
|---|---|
| No chrome, no title bar, centred, ~640 px | It's a lens, not a window. Raycast and Spotlight established the vocabulary; fighting it costs the user nothing but familiarity |
| Results appear **as you type**, lexical first | Never a spinner. The index answers in single-digit ms ([M0](../bench/M0-corpus.md)) |
| `⌘1–9` jump directly | Hands stay on the keyboard |
| Dismisses on Escape or blur, **remembers nothing** | A quick-find that persists state becomes a window you have to manage |

### 3.2 Main window — for working, not just finding

Three panes. Familiar from Mail, Linear, Finder column view — familiar is correct here, because attention should go to the content.

```
┌──────────┬────────────────────────────┬──────────────────────────┐
│          │  ⌕ auth refresh token      │                          │
│  Search  ├────────────────────────────┤  token.rs                │
│  Files   │  token.rs        exact·2d  │  src/auth/token.rs       │
│  Ask     │  src/auth/                 │                          │
│  Activity│  ──────────────────────    │  ┌────────────────────┐  │
│          │  auth-design.md  sem ·3w   │  │ 140  }             │  │
│  ────    │  docs/                     │  │ 141                │  │
│  melp    │  ──────────────────────    │  │ 142  pub async fn  │  │
│  pictures│  standup.md      sem ·8w   │  │ 143    let claims  │  │
│          │  notes/                    │  └────────────────────┘  │
│  ────    │                            │                          │
│  ⚙       │  17 results · 8 ms         │  IDENTITY   METADATA ▾   │
└──────────┴────────────────────────────┴──────────────────────────┘
   180px            360px                        remainder
```

| Pane | Role |
|---|---|
| **Sidebar** | Navigation + workspaces + health. Collapsible to icons. Workspace rows show live state — a degraded watcher is visible without navigating |
| **List** | Results or file browser. Virtualized. Keyboard-navigable with `j/k` and arrows |
| **Detail** | Preview with the match highlighted in context, plus the file-intelligence panel below the fold |

---

## 4. Information architecture

Part 1 §16.1 trimmed to what actually ships, in milestone order.

| View | Ships | Purpose |
|---|---|---|
| **Search** | M2 | The default view. Hybrid results, match reasons, preview |
| **Files** | M2 | Browse by workspace/folder. The index as a filesystem you can trust |
| **File detail** | M2 | The `FI` panel — everything known about one file |
| **Status** | M2 | Index health, queue, errors, cloud-only counts |
| **Workspaces** | M2 | Add/remove roots, per-root policy (gitignore, hidden, media) |
| **Ask** | M4 | Cited answers |
| **Activity** | M6 | Timeline: what changed, Git events |
| **Settings** | M2 | Models, privacy, indexing, appearance |
| ~~Knowledge~~ | D43 | Graph explorer. Gated — may never ship |
| ~~Agents~~ | M5 | Recipes. Only if E1 recipes prove useful |

**Search is the launch view.** Not a dashboard, not an empty state with tips. The app opens focused in the search field.

---

## 5. Interaction design

### 5.1 Keyboard is the primary input

This is a tool for someone who lives in a terminal. Mouse support is table stakes; keyboard support is the product.

| Key | Action |
|---|---|
| `⌘K` / `⌘P` | Command palette |
| `⌘F` | Focus search |
| `⌥Space` | Quick-find overlay (global, works when app is not focused) |
| `j` / `k` / `↑` / `↓` | Move selection |
| `↵` | Open in default app |
| `⌘↵` | Open in `$EDITOR` at the exact line |
| `⇧↵` | Reveal in Finder |
| `⌘C` | Copy the citation (`path:line`) |
| `⌘1–9` | Jump to result *n* |
| `Tab` | Cycle panes |
| `⌘\` | Toggle sidebar |
| `Esc` | Clear / close / back |
| `?` | Shortcuts |

**Every action reachable by mouse is reachable by keyboard.** Enforced by review, not aspiration.

### 5.2 Results stream, they don't load

The single most important interaction rule, inherited from [UX §2](UX.md) principle 1.

```
 t=0     keystroke
 t=8ms   lexical results render          ← user can already act
 t=140ms semantic results merge in place
```

| Rule |
|---|
| **No spinner ever gates a result that is already known** |
| Semantic results **merge into the existing list**, they don't replace it. Rows animate to new positions over ~120 ms so the eye tracks the change |
| The row under the cursor **never moves** during a re-rank. Selection is anchored to the result, not the index |
| A slow branch shows a subtle inline indicator in the footer, not a modal |

That third rule matters more than it looks: a list that re-orders under an active selection is how you make someone open the wrong file.

### 5.3 Empty and degraded states are diagnoses

Same principle as [UX §4](UX.md). A zero-result search shows what *isn't* indexed and offers the fix inline:

```
      No matches for "quarterly revenue"

      Indexed        9,435 files · 2 workspaces
      Not indexed    412 cloud-only files in iCloud       [ Index them ]
                     66 xlsx hidden by .gitignore in melp [ Include ]
```

Buttons, not instructions. The state that caused the problem is the state that can fix it.

### 5.4 Provenance is always on screen

The differentiator is verifiable citation. If checking a source takes more than one action, nobody checks and the provenance is decorative.

| Element | Treatment |
|---|---|
| Match reason | Badge on every row: `exact` `semantic` `path` `recent` |
| Provenance class | `~approx` badge on anything not `Exact`; row tinted subtly |
| Origin | `self` badge on agent-written files, which **cannot** be cited (the `origin = SELF` rule) |
| Citation | One click reveals the exact page/cell/line, highlighted in context |
| Extraction method | Shown beside every derived fact in the detail panel |

---

## 6. Visual direction

**Calm, dense, native.** This is an instrument, not a destination. Nothing should ask for attention except state that needs it.

| Axis | Choice | Reason |
|---|---|---|
| Density | Compact. 28 px rows, 13 px base | It's a list tool. Airy spacing means less information and more scrolling |
| Type | SF Pro (UI) · SF Mono (paths, code, hashes) | Native on the target platform; monospace where alignment carries meaning |
| Colour | Near-monochrome base; colour reserved for **state and match reason** | If everything is coloured, nothing reads as important |
| Accent | One. Used for selection and focus only | |
| Elevation | Borders and background steps, not shadows | Shadows read as web; native macOS uses separation |
| Motion | ≤ 150 ms, only for re-rank and pane transitions | Motion communicates *change*. Decorative motion in a search tool is latency you added on purpose |
| Corner radius | 6 px | Matches platform convention |
| Theme | Light + dark, follows system | Non-negotiable on macOS |

### Tokens

```
--bg          canvas          --text        primary
--bg-raised   panels, rows    --text-dim    metadata, paths
--bg-hover                    --text-faint  breadcrumbs, timing
--border      hairlines       --accent      selection, focus
--border-strong               --warn        degraded state
                              --error       failures
```

**Colour never carries meaning alone** (A11Y-003). Every badge is text + colour; every state has a glyph.

---

## 7. Performance budgets

A local tool that feels slower than `grep` has no reason to exist.

| Interaction | Budget |
|---|---|
| Keystroke → first lexical result | **< 50 ms** |
| Keystroke → rendered frame | < 16 ms (60 fps while typing) |
| Semantic results merged | < 300 ms |
| File detail panel open | < 100 ms |
| Quick-find overlay appear | < 80 ms from hotkey |
| App cold start → usable search | **< 800 ms** |
| Idle memory (WebView + core) | < 250 MB |
| Scroll 10k results | 60 fps, virtualized |

| Rule |
|---|
| Search input is **not** debounced beyond 30 ms. The index is faster than the debounce would be |
| The result list is virtualized from the first row, not after some threshold |
| Preview content is bounded — a 50 MB file renders its matched region, never the whole file |
| No layout thrash on re-rank: rows are absolutely positioned and transformed |

---

## 8. Accessibility

Not deferred. Retrofitting is more expensive than building it in, and the keyboard-first design does most of the work already.

- Full keyboard reachability, visible focus rings, logical tab order
- Semantic HTML; ARIA only where a native element can't express the role
- Live region announces result counts and state changes
- Respects `prefers-reduced-motion` — re-rank becomes instant, not animated
- Respects `prefers-contrast` and system text size
- WCAG AA contrast minimum on every token pair
- Colour never the sole carrier of meaning
- Screen-reader labels on every icon-only control

---

## 9. Deliberately not built

| Not building | Why |
|---|---|
| A chat interface with an agent loop | Part 7 §130 stands. Ask is a query surface with citations, not a conversation with tool-calling |
| Model management UI beyond selection | Ollama has one |
| A graph visualization | D43 gates the graph itself. Part 1 §16.5 already warns against the hairball |
| Theming / customization | One good theme in two modes |
| Onboarding tour | Add a folder, search it. If that needs a tour, the design failed |
| Plugin UI | Not until there are plugins |
| Multi-window | One window plus the overlay |

---

## 10. Milestone impact

The GUI reorders the roadmap. **Open decision — see D48.**

| M | Was | Now |
|---|---|---|
| M1 | Index + query | unchanged |
| M2 | MCP server | **MCP server** *(recommended: keep)* — 1–2 weeks, and it validates the whole query API end-to-end against a real consumer before any UI is built on top of it |
| M3 | PDF + tables | **Desktop shell + Search + Files + File detail + Status** |
| M4 | Semantic | Semantic + Ask view |
| M5 | Write tools | Write tools + approval UI |
| M6 | Timeline | Timeline + Activity view |

**Why MCP still goes first:** it is the cheapest possible end-to-end test of the query layer. A bad query API discovered through MCP costs a day; discovered after three panes are built on it, it costs a rewrite. It also means that if the GUI slips, the index is still useful.

The counter-argument is motivation — risk S7 says interest fades before something is usable daily, and a GUI is more motivating than a stdio server. That's a real consideration and it's the user's call, not mine.

---

## 11. What "production grade" means for the GUI

- [ ] First result renders in < 50 ms, every time, regardless of semantic state
- [ ] Cold start to usable search < 800 ms
- [ ] Selection never moves under the user during a re-rank
- [ ] Every mouse action has a keyboard equivalent
- [ ] Every degraded state is visible from the sidebar without navigating
- [ ] Every citation is one action from its exact source location
- [ ] Zero results explains itself and offers the fix as a button
- [ ] Light and dark both pass WCAG AA
- [ ] `prefers-reduced-motion` honoured
- [ ] WebView has no filesystem, shell or network capability
- [ ] TypeScript command types generated from Rust, drift-checked in CI
- [ ] 60 fps scrolling a 10k-row virtualized list
