/**
 * The window.
 *
 * Every keybinding this app has is registered here, in one place, and every one
 * of them calls the same function the mouse calls. That is most of what makes
 * "every action reachable by mouse is reachable by keyboard" true by
 * construction rather than by review — the rest is not swallowing Tab in views
 * that have nothing to do with it, which is the other half of the rule and the
 * half this file used to break.
 *
 * It is not the whole of GUI §5.1. The command palette that section lists is
 * not built, and `ShortcutsDialog` says so rather than quietly relabelling the
 * key next to it.
 *
 * The open verbs, after `open_path` / `reveal_path` arrived — Enclave's
 * *peek before open*, which is the pattern this layout already implies:
 *
 *   ↓ ↑ j k   move the cursor; the detail pane follows it. This is the peek,
 *             and it costs nothing, so it happens continuously.
 *   ↵         read it here: focus the preview pane.
 *   ⌘↵        hand the file to the system's default application.
 *   ⇧↵        reveal it in the file manager.
 *
 * None of the three says "that command does not exist" any more.
 */

import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import styles from "./App.module.css";
import { TitleBar } from "./components/TitleBar";
import { Sidebar } from "./components/Sidebar";
import { ViewSwitcher } from "./components/ViewSwitcher";
import { SearchView } from "./components/SearchView";
import { FilesView } from "./components/FilesView";
import { AskView } from "./components/AskView";
import { ModelsView } from "./components/ModelsView";
import { StatusView } from "./components/StatusView";
import { SettingsView } from "./components/SettingsView";
import { ArtifactPanel } from "./components/ArtifactPanel";
import { QuickFind } from "./components/QuickFind";
import { ShortcutsDialog } from "./components/ShortcutsDialog";
import { Welcome, useSetupGate } from "./components/Welcome";
import { DropZone } from "./components/DropZone";
import { Notice } from "./components/Notice";
import { cx } from "./lib/cx";
import { baseOf, count } from "./lib/format";
import {
  useDebounced,
  useIndexHealth,
  useSearch,
  useSettledFlag,
  useWorkspaces,
} from "./queries";
import { anchorOf, hitKey, panesFor, useUi, VIEWS } from "./store";
import {
  copyCitation,
  grantFolder,
  openInSystem,
  pickFiles,
  revealInFileManager,
} from "./actions";
import type { SearchHit } from "./api";

/** How long a fetch may run before the footer admits it is running (GUI §5.2). */
const SLOW_AFTER_MS = 150;

/** A stable empty ranking, so "no results" is not a new array every render. */
const NO_HITS: readonly SearchHit[] = [];

export function App() {
  const view = useUi((s) => s.view);
  const query = useUi((s) => s.query);
  const setQuery = useUi((s) => s.setQuery);
  const anchor = useUi((s) => s.anchor);
  const setAnchor = useUi((s) => s.setAnchor);
  const focusedPane = useUi((s) => s.focusedPane);
  const focusPane = useUi((s) => s.focusPane);
  const cyclePane = useUi((s) => s.cyclePane);
  const toggleSidebar = useUi((s) => s.toggleSidebar);
  const collapsed = useUi((s) => s.sidebarCollapsed);

  const debounced = useDebounced(query);
  const searchQ = useSearch(debounced);
  const hits = useMemo<readonly SearchHit[]>(
    () => searchQ.data?.hits ?? NO_HITS,
    [searchQ.data],
  );
  const slow = useSettledFlag(searchQ.isFetching, SLOW_AFTER_MS);

  const health = useIndexHealth();
  const workspaces = useWorkspaces();
  /*
   * Whether the setup dialog is up, and therefore owns the keyboard.
   *
   * Decided here rather than inside the dialog so there is one answer, and so
   * the dialog is *mounted* only while it is open — it reads the Models
   * snapshot, which polls every four seconds, and mounting it always would put
   * that poll behind every screen in the app to render nothing.
   */
  const setupVisible = useSetupGate();

  const searchRef = useRef<HTMLInputElement>(null);
  const sidebarRef = useRef<HTMLElement>(null);
  const resultsRef = useRef<HTMLDivElement>(null);
  const detailRef = useRef<HTMLDivElement>(null);
  /** The content sheet, as a place to put focus when a view opens. */
  const sheetRef = useRef<HTMLDivElement>(null);

  /** Bumped only when a keypress moved the cursor, so a re-rank never scrolls. */
  const [scrollNonce, setScrollNonce] = useState(0);

  const selectedIndex = anchor
    ? hits.findIndex((h) => hitKey(h) === anchor.key)
    : -1;
  const selectedHit = selectedIndex >= 0 ? (hits[selectedIndex] ?? null) : null;

  /*
   * Selection maintenance — GUI §5.2.
   *
   * If the anchored result is still in the ranking, nothing happens: the row
   * moved, the cursor did not. Only when the anchored result is *gone* — a new
   * query, or a result that dropped out — does the cursor go to the top.
   */
  useEffect(() => {
    if (hits.length === 0) {
      if (anchor !== null && query.trim() === "") setAnchor(null);
      return;
    }
    if (anchor !== null && hits.some((h) => hitKey(h) === anchor.key)) return;
    const first = hits[0];
    if (first) setAnchor(anchorOf(first));
  }, [hits, anchor, query, setAnchor]);

  /* Announce the ranking to a screen reader (GUI §8, live region). */
  const announce = useUi((s) => s.announce);
  useEffect(() => {
    const r = searchQ.data;
    if (!r || r.query === "") return;
    announce(
      `${count(r.matched)} ${r.matched === 1 ? "result" : "results"} for ${r.query}`,
    );
  }, [searchQ.data, announce]);

  /* ── the verbs ─────────────────────────────────────────────────────────── */

  const select = useCallback(
    (hit: SearchHit) => setAnchor(anchorOf(hit)),
    [setAnchor],
  );

  /** `↵` — read it here. The pane is already showing it; this hands it focus. */
  const peek = useCallback(
    (hit: SearchHit) => {
      setAnchor(anchorOf(hit));
      focusPane("detail");
    },
    [setAnchor, focusPane],
  );

  const move = useCallback(
    (delta: number) => {
      if (hits.length === 0) return;
      const from = selectedIndex >= 0 ? selectedIndex : 0;
      const next = Math.min(Math.max(from + delta, 0), hits.length - 1);
      const hit = hits[next];
      if (!hit) return;
      setAnchor(anchorOf(hit));
      setScrollNonce((n) => n + 1);
    },
    [hits, selectedIndex, setAnchor],
  );

  /* ── keyboard ──────────────────────────────────────────────────────────── */

  // The handler reads live values through a ref so it can be bound once.
  const live = useRef({ hits, anchor, move, peek, select, query, setupVisible });
  live.current = { hits, anchor, move, peek, select, query, setupVisible };

  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      const meta = e.metaKey || e.ctrlKey;
      const target = e.target as HTMLElement | null;
      const typing =
        target !== null &&
        (target.tagName === "INPUT" ||
          target.tagName === "TEXTAREA" ||
          target.isContentEditable);
      const ui = useUi.getState();
      const s = live.current;

      // ⌘K reaches through the overlay so the same key closes it.
      if (meta && e.key.toLowerCase() === "k") {
        e.preventDefault();
        if (ui.quickFindOpen) ui.closeQuickFind();
        else ui.openQuickFind();
        return;
      }
      /*
       * ⌘O and ⇧⌘O — add files, add a folder.
       *
       * **A drop is a mouse gesture with no keyboard equivalent at all**, and
       * GUI §11 has no exception for gestures the OS invented. These are it:
       * both open a native panel in Rust, which is the same path the drop takes
       * once the paths exist. "Add a folder" had no key either — it was a
       * button on one page and nothing else.
       *
       * Bound above the overlay guard so they still work while the setup dialog
       * is up, because the dialog exists to offer exactly these two things.
       */
      if (meta && e.key.toLowerCase() === "o") {
        e.preventDefault();
        if (e.shiftKey) void grantFolder();
        else void pickFiles();
        return;
      }
      // The overlay and the dialog own every other key while they are up. The
      // setup dialog is modal too: it handles Escape and Tab itself, and the
      // verbs below act on a ranking that is behind it.
      if (ui.quickFindOpen || ui.shortcutsOpen || s.setupVisible) return;

      if (meta && e.key.toLowerCase() === "f") {
        e.preventDefault();
        ui.setView("search");
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      // ⌘N — a fresh thread, from anywhere. The one action on the rail that is
      // not a destination, so it is the one that earns a global key.
      if (meta && e.key.toLowerCase() === "n") {
        e.preventDefault();
        ui.startConversation();
        return;
      }
      if (meta && e.key === "\\") {
        e.preventDefault();
        toggleSidebar();
        return;
      }
      /*
       * ⌘⌥1–6 — the six sections, counted in `VIEWS` order.
       *
       * Three spellings were possible and two of them are already taken:
       *
       *  - `⌘1-6` is jump-to-result, right below, and has been since there were
       *    results to jump to. Making it mean something else *outside* Search
       *    was the tempting third option and it is the one this file argues
       *    against a few lines down — a key that does two things at once does
       *    neither predictably, and "which one did I get" is answered by
       *    remembering which screen you are on.
       *  - `⌃1-6` collides too, invisibly: `meta` here is `metaKey || ctrlKey`,
       *    so `⌃1` already *is* `⌘1` to this handler and would jump to the
       *    first result. Un-aliasing Ctrl for one binding and not the rest is a
       *    worse trade than the extra modifier.
       *
       * Read from `e.code`, never `e.key`. On macOS Option rewrites the
       * character — `⌥1` is `¡`, `⌥2` is `™` — so `e.key` never sees a digit
       * and a `/^[1-9]$/` test here would silently match nothing.
       *
       * Before the result-jump check so the two can never be ambiguous, even
       * on a layout where Option leaves the digit alone.
       */
      if (meta && e.altKey && e.code.startsWith("Digit")) {
        const next = VIEWS[Number(e.code.slice("Digit".length)) - 1];
        if (next) {
          e.preventDefault();
          ui.setView(next);
        }
        return;
      }
      // ⌘, — Settings. The macOS convention, and the one place a user will try
      // a shortcut before looking for a button.
      if (meta && !e.altKey && e.key === ",") {
        e.preventDefault();
        ui.setView("settings");
        return;
      }
      if (meta && /^[1-9]$/.test(e.key)) {
        e.preventDefault();
        const hit = s.hits[Number(e.key) - 1];
        if (hit) {
          s.peek(hit);
          setScrollNonce((n) => n + 1);
        }
        return;
      }
      if (meta && e.key.toLowerCase() === "c") {
        // Never steal a real text selection: ⌘C on selected text is ⌘C.
        if ((window.getSelection()?.toString() ?? "") !== "") return;
        if (s.anchor) {
          e.preventDefault();
          void copyCitation(s.anchor);
        }
        return;
      }
      // Esc closes the artifact panel first: it is the newest thing on screen
      // and the one the key is most likely aimed at. Not while typing, because
      // in the composer Esc already means "stop generating", and a key that
      // does two things at once does neither predictably. The panel handles
      // Esc itself whenever focus is inside it, so the control is never more
      // than one Tab away.
      if (e.key === "Escape" && ui.view === "ask" && ui.artifact !== null && !typing) {
        e.preventDefault();
        ui.closeArtifact();
        return;
      }
      /*
       * Escape belongs to the view that has something to clear.
       *
       * It used to be unguarded: from *any* screen it wiped the search query —
       * a query nothing on that screen was showing — and when the query was
       * already empty it blurred whatever had focus, which on Ask is the
       * composer someone is mid-sentence in. Ask needs the key for "stop
       * generating", the artifact panel and the dialogs handle their own, and
       * nowhere else has a query at all. So: Search clears, everyone else keeps
       * it.
       */
      if (e.key === "Escape") {
        if (ui.view !== "search") return;
        e.preventDefault();
        if (s.query !== "") ui.setQuery("");
        // Either way focus lands back on the field, which is where the next
        // keystroke is meant to go. The old blur-then-focus pair was the same
        // thing said twice.
        searchRef.current?.focus();
        return;
      }
      if (e.key === "Tab") {
        /*
         * Tab is the browser's unless this view genuinely has panes to cycle.
         *
         * The reasoning that released it for Ask holds everywhere the pane
         * model does not apply, and it applies in two views. Ask has no panes;
         * Models, Status and Settings have none either, and swallowing Tab
         * there focused `null` — which is why the API key could not be typed
         * into by keyboard. Files had the cycle stepping onto a `resultsRef`
         * nothing attaches. Native tab order is what a browser hands you for
         * free, and every one of those pages is a column of ordinary controls
         * that it orders correctly.
         *
         * `panesFor` is the same question `cyclePane` asks itself, so the key
         * and the action cannot disagree. In the two views that keep the cycle
         * the switcher is off the tab path, which is what `⌘⌥1-6` above is for.
         */
        if (panesFor(ui.view, ui.sidebarCollapsed).length < 2) return;
        e.preventDefault();
        cyclePane(e.shiftKey);
        return;
      }
      if (e.key === "?" && !typing) {
        e.preventDefault();
        ui.setShortcutsOpen(true);
        return;
      }
      /*
       * The verbs below move and act on the search ranking, and Ask has no
       * ranking on screen — so in that view they are not shortcuts, they are
       * three keys taken away from a surface that needs all of them. `↵` is how
       * a button is pressed, which is how an artifact card is opened; the arrows
       * are how a scroller is scrolled, which is how a long generated page is
       * read. Neither had a keyboard equivalent while this swallowed them.
       */
      const inAsk = ui.view === "ask";
      if (!inAsk && (e.key === "ArrowDown" || (!typing && e.key === "j"))) {
        e.preventDefault();
        s.move(1);
        return;
      }
      if (!inAsk && (e.key === "ArrowUp" || (!typing && e.key === "k"))) {
        e.preventDefault();
        s.move(-1);
        return;
      }
      if (e.key === "Enter" && !inAsk) {
        // The anchor, not the hit: `⌘↵` and `⇧↵` must keep working when the
        // selected file has dropped out of the current ranking.
        const a = s.anchor;
        if (!a) return;
        e.preventDefault();
        const label = baseOf(a.relativePath);
        if (e.shiftKey) {
          void revealInFileManager(a.path, label);
        } else if (meta) {
          void openInSystem(a.path, label);
        } else {
          const hit = s.hits.find((h) => hitKey(h) === a.key);
          if (hit) s.peek(hit);
          else useUi.getState().focusPane("detail");
        }
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [cyclePane, toggleSidebar]);

  /* ── focus ─────────────────────────────────────────────────────────────── */

  // "The app opens focused in the search field" (GUI §4).
  useEffect(() => {
    searchRef.current?.focus();
  }, []);

  /*
   * Opening a view puts focus inside it.
   *
   * Without this, clicking a section left focus on the switcher button, so the
   * first Tab in the new view started from the chrome and walked back through
   * it. Pressing `⌘⌥n` was worse: the control focus was on has just been
   * unmounted, so focus falls to `<body>` and Tab starts from the top of the
   * document.
   *
   * **Only focus that is idle is taken.** A view that claims its own focus on
   * arrival has already decided better than this effect can — Ask focuses its
   * composer on every conversation epoch, and `⌘N` is exactly that path — so
   * anything already holding focus keeps it. What counts as idle is the
   * switcher itself, and nothing at all.
   */
  useEffect(() => {
    const active = document.activeElement as HTMLElement | null;
    const idle =
      active === null ||
      active === document.body ||
      active.closest("[data-switcher]") !== null;
    if (!idle) return;
    // Search opens in its field, which is GUI §4's opening state and what ⌘F
    // does. Everywhere else the sheet takes it: a container, not a control, so
    // arriving on a page never types into it or presses anything, and the next
    // Tab starts at the top of the view rather than at the top of the window.
    if (view === "search") searchRef.current?.focus();
    else sheetRef.current?.focus();
  }, [view]);

  const mounted = useRef(false);
  useEffect(() => {
    if (!mounted.current) {
      mounted.current = true;
      return;
    }
    const el =
      focusedPane === "sidebar"
        ? sidebarRef.current
        : focusedPane === "results"
          ? resultsRef.current
          : detailRef.current;
    el?.focus();
  }, [focusedPane]);

  /* ── render ────────────────────────────────────────────────────────────── */

  const indexed = health.data;
  const spaces = workspaces.data?.length;
  const idle =
    indexed === undefined || spaces === undefined
      ? null
      : `${count(indexed.files)} files indexed across ${count(spaces)} ${
          spaces === 1 ? "workspace" : "workspaces"
        }`;

  return (
    <div className={styles.app}>
      <TitleBar>
        <ViewSwitcher />
      </TitleBar>

      <div className={cx(styles.body, collapsed && styles.collapsed)}>
        <Sidebar ref={sidebarRef} />

        {/*
          One sheet, and every view is a flex row inside it. The sheet clips,
          so a pane that miscalculates its own height can no longer paint over
          the window edge — the failure the user saw as "lower part is hidden".
        */}
        {/* `tabIndex={-1}` makes it a focus target without putting it in the
            tab order: it is where focus lands when a view opens, and nothing
            more. */}
        <div ref={sheetRef} tabIndex={-1} className={styles.sheet}>
          {view === "search" && (
            <SearchView
              query={query}
              onQueryChange={setQuery}
              response={searchQ.data}
              error={searchQ.error ?? null}
              slow={slow}
              anchor={anchor}
              selectedHit={selectedHit}
              scrollNonce={scrollNonce}
              onSelect={select}
              onOpen={peek}
              searchRef={searchRef}
              resultsRef={resultsRef}
              detailRef={detailRef}
              idle={idle}
            />
          )}
          {view === "files" && <FilesView detailRef={detailRef} />}

          {/*
            **Ask stays mounted.** Every other view is a report and can be
            rebuilt from a query; a conversation cannot. Unmounting it threw
            away the whole thread on a tab switch — and worse, a generation in
            flight kept running with nothing left to receive its tokens, so the
            answer was lost even though the model produced it. Hidden rather
            than unmounted, so switching to Status and back is a navigation
            rather than a data loss.
          */}
          <div hidden={view !== "ask"} className={styles.keepAlive}>
            <AskView />
            {/*
              A sibling of the conversation, not a child of it. The artifact
              belongs to the answer, but it is a *column*: nested inside the
              thread it could only ever be as tall as a message and as wide as
              the measure, which is the shape it was already failing in. Here
              the conversation simply gets narrower, and closing gives the
              width straight back.
            */}
            <ArtifactPanel />
          </div>
          {view === "models" && <ModelsView />}
          {view === "status" && <StatusView />}
          {view === "settings" && <SettingsView />}
        </div>
      </div>

      <QuickFind />
      <ShortcutsDialog />
      {setupVisible && <Welcome />}
      {/*
        Last, so it paints over everything including the setup dialog: dropping
        a file while that dialog is up is the most likely place someone will
        try it, and a drop target hidden behind the thing telling you to drop
        is the worst possible ordering.
      */}
      <DropZone />
      <Notice />
    </div>
  );
}
