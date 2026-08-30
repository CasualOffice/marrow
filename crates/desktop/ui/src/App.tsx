/**
 * The window.
 *
 * Every keybinding in GUI §5.1 is registered here, in one place, and every one
 * of them calls the same function the mouse calls. That is what makes "every
 * action reachable by mouse is reachable by keyboard" true by construction
 * rather than by review.
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
import { SearchView } from "./components/SearchView";
import { FilesView } from "./components/FilesView";
import { AskView } from "./components/AskView";
import { ModelsView } from "./components/ModelsView";
import { StatusView } from "./components/StatusView";
import { SettingsView } from "./components/SettingsView";
import { QuickFind } from "./components/QuickFind";
import { ShortcutsDialog } from "./components/ShortcutsDialog";
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
import { anchorOf, hitKey, useUi, type View } from "./store";
import { copyCitation, openInSystem, revealInFileManager } from "./actions";
import type { SearchHit } from "./api";

/** How long a fetch may run before the footer admits it is running (GUI §5.2). */
const SLOW_AFTER_MS = 150;

/** A stable empty ranking, so "no results" is not a new array every render. */
const NO_HITS: readonly SearchHit[] = [];

const TITLES: Record<View, string> = {
  search: "Marrow",
  ask: "Ask",
  files: "Files",
  models: "Models",
  status: "Status",
  settings: "Settings",
};

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

  const searchRef = useRef<HTMLInputElement>(null);
  const sidebarRef = useRef<HTMLElement>(null);
  const resultsRef = useRef<HTMLDivElement>(null);
  const detailRef = useRef<HTMLDivElement>(null);

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
  const live = useRef({ hits, anchor, move, peek, select, query });
  live.current = { hits, anchor, move, peek, select, query };

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
      // The overlay and the dialog own every other key while they are up.
      if (ui.quickFindOpen || ui.shortcutsOpen) return;

      if (meta && e.key.toLowerCase() === "f") {
        e.preventDefault();
        ui.setView("search");
        searchRef.current?.focus();
        searchRef.current?.select();
        return;
      }
      if (meta && e.key === "\\") {
        e.preventDefault();
        toggleSidebar();
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
      if (e.key === "Escape") {
        e.preventDefault();
        if (s.query !== "") {
          ui.setQuery("");
          searchRef.current?.focus();
        } else {
          (document.activeElement as HTMLElement | null)?.blur();
          searchRef.current?.focus();
        }
        return;
      }
      if (e.key === "Tab") {
        e.preventDefault();
        cyclePane(e.shiftKey);
        return;
      }
      if (e.key === "?" && !typing) {
        e.preventDefault();
        ui.setShortcutsOpen(true);
        return;
      }
      if (e.key === "ArrowDown" || (!typing && e.key === "j")) {
        e.preventDefault();
        s.move(1);
        return;
      }
      if (e.key === "ArrowUp" || (!typing && e.key === "k")) {
        e.preventDefault();
        s.move(-1);
        return;
      }
      if (e.key === "Enter") {
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
      <TitleBar title={TITLES[view]} />

      <div className={cx(styles.body, collapsed && styles.collapsed)}>
        <Sidebar ref={sidebarRef} />

        {/*
          One sheet, and every view is a flex row inside it. The sheet clips,
          so a pane that miscalculates its own height can no longer paint over
          the window edge — the failure the user saw as "lower part is hidden".
        */}
        <div className={styles.sheet}>
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
          </div>
          {view === "models" && <ModelsView />}
          {view === "status" && <StatusView />}
          {view === "settings" && <SettingsView />}
        </div>
      </div>

      <QuickFind />
      <ShortcutsDialog />
      <Notice />
    </div>
  );
}
