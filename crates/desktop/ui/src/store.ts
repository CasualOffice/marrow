/**
 * UI-only state (GUI §2: "Never mirrors core state").
 *
 * Nothing the core knows lives here — no hits, no counts, no health. What lives
 * here is what the *window* knows: which pane has focus, what is typed, and
 * which result the cursor is anchored to.
 */

import { create } from "zustand";
import type { SearchHit } from "./api";
import { applyTheme, loadTheme, saveTheme, type ThemeChoice } from "./theme";

export type View = "search" | "ask" | "files" | "models" | "status" | "settings";
export type Pane = "sidebar" | "results" | "detail";

/**
 * The selection anchor — GUI §5.2, the single most important interaction rule.
 *
 * Selection is stored as the *identity* of a result (`fileId` + `line`), never
 * as an index into the array. When semantic results merge in and the ranking
 * changes, the cursor is still on the same file at the same line; the row moves
 * under it, the cursor does not move to a different row. A list that re-orders
 * under an active selection is how you make someone open the wrong file.
 *
 * `path` and `line` ride along so the detail pane keeps rendering even if the
 * selected result drops out of the new ranking entirely.
 */
export interface Anchor {
  readonly key: string;
  readonly path: string;
  readonly relativePath: string;
  readonly line: number | null;
  readonly location: string;
}

/** The identity of a result. Stable across re-ranks; not derived from order. */
export function hitKey(h: Pick<SearchHit, "fileId" | "line">): string {
  return `${h.fileId}#${h.line ?? "-"}`;
}

export function anchorOf(h: SearchHit): Anchor {
  return {
    key: hitKey(h),
    path: h.path,
    relativePath: h.relativePath,
    line: h.line,
    location: h.location,
  };
}

interface UiState {
  view: View;
  query: string;
  sidebarCollapsed: boolean;
  focusedPane: Pane;

  /**
   * Which workspace the Files view is scoped to, by name. `null` is "all".
   *
   * UI state, not core state: it is a question this window is asking, and the
   * answer comes back from `list_files` each time it changes.
   */
  workspaceFilter: string | null;

  /** Appearance. Local to this window; persisted to localStorage. */
  theme: ThemeChoice;

  /** Anchored to result identity, never to an index. */
  anchor: Anchor | null;

  quickFindOpen: boolean;
  quickFindQuery: string;
  shortcutsOpen: boolean;

  /** Transient footer message. Also mirrored into the live region. */
  notice: { text: string; id: number } | null;
  /** Announced to screen readers: result counts and state changes (GUI §8). */
  announcement: string;

  setView: (v: View) => void;
  setQuery: (q: string) => void;
  setWorkspaceFilter: (name: string | null) => void;
  setTheme: (t: ThemeChoice) => void;
  toggleSidebar: () => void;
  focusPane: (p: Pane) => void;
  cyclePane: (back: boolean) => void;
  setAnchor: (a: Anchor | null) => void;

  openQuickFind: () => void;
  closeQuickFind: () => void;
  setQuickFindQuery: (q: string) => void;
  setShortcutsOpen: (open: boolean) => void;

  notify: (text: string) => void;
  announce: (text: string) => void;
}

let noticeId = 0;

/** Tab order across panes. The sidebar drops out when it is collapsed. */
const PANES: Pane[] = ["sidebar", "results", "detail"];

export const useUi = create<UiState>((set, get) => ({
  view: "search",
  query: "",
  workspaceFilter: null,
  theme: loadTheme(),
  sidebarCollapsed: false,
  focusedPane: "results",
  anchor: null,
  quickFindOpen: false,
  quickFindQuery: "",
  shortcutsOpen: false,
  notice: null,
  announcement: "",

  setView: (view) => set({ view }),
  setQuery: (query) => set({ query }),
  setWorkspaceFilter: (workspaceFilter) => set({ workspaceFilter }),
  setTheme: (theme) => {
    saveTheme(theme);
    applyTheme(theme);
    set({ theme });
  },
  toggleSidebar: () =>
    set((s) => ({
      sidebarCollapsed: !s.sidebarCollapsed,
      focusedPane:
        !s.sidebarCollapsed && s.focusedPane === "sidebar"
          ? "results"
          : s.focusedPane,
    })),
  focusPane: (focusedPane) => set({ focusedPane }),
  cyclePane: (back) =>
    set((s) => {
      const panes = s.sidebarCollapsed
        ? PANES.filter((p) => p !== "sidebar")
        : PANES;
      const i = panes.indexOf(s.focusedPane);
      const next = panes[(i + (back ? -1 : 1) + panes.length) % panes.length];
      return { focusedPane: next ?? "results" };
    }),
  setAnchor: (anchor) => set({ anchor }),

  openQuickFind: () => set({ quickFindOpen: true }),
  closeQuickFind: () => set({ quickFindOpen: false, quickFindQuery: "" }),
  setQuickFindQuery: (quickFindQuery) => set({ quickFindQuery }),
  setShortcutsOpen: (shortcutsOpen) => set({ shortcutsOpen }),

  notify: (text) => {
    noticeId += 1;
    set({ notice: { text, id: noticeId }, announcement: text });
  },
  announce: (announcement) => {
    if (get().announcement !== announcement) set({ announcement });
  },
}));
