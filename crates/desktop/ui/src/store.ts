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
 * A generated page or diagram, while it is open in the side panel.
 *
 * This is the one piece of window state whose two ends have no common ancestor
 * worth threading a prop through: the thing is produced deep inside a streamed
 * answer, inside a scroller, inside the Ask view, and it is *shown* by a panel
 * that is a sibling of that whole view. Everything between them would be
 * carrying a prop it has no opinion about.
 */
export type ArtifactKind = "html" | "mermaid";

/** Rendered as the thing it is, or as the source it was written as. */
export type ArtifactMode = "preview" | "source";

export interface Artifact {
  /**
   * The identity of the block that produced it — stable for as long as that
   * answer is on screen, and unique across answers. Opening a second artifact
   * replaces the first rather than stacking, so this is what tells the panel
   * whether an update belongs to what it is showing.
   */
  readonly key: string;
  readonly kind: ArtifactKind;
  readonly title: string;
  readonly source: string;
  /** True while the answer writing it is still streaming. */
  readonly streaming: boolean;
}

/*
 * Panel geometry.
 *
 * `MIN` is the narrowest a generated page is worth looking at; below it the
 * panel is costing the conversation more than it returns. `CONVERSATION_MIN` is
 * the floor the answer column may not be pushed under — about 56 characters,
 * against the 62 of `--prose-measure` — because a panel that wins every pixel
 * has covered the thing it was opened to explain.
 *
 * Their sum is the width at which side-by-side stops being possible at all, and
 * the panel takes the sheet over instead. `ArtifactPanel.module.css` states that
 * same number as a container query; the two are commented at each other because
 * CSS cannot read a constant.
 */
export const ARTIFACT_W_MIN = 320;
export const ARTIFACT_W_MAX = 760;
export const ARTIFACT_W_DEFAULT = 460;
export const CONVERSATION_W_MIN = 420;

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

  /** The generated page or diagram on show beside the conversation. */
  artifact: Artifact | null;
  artifactMode: ArtifactMode;
  /**
   * The panel's width in pixels. Session-only and deliberately not persisted:
   * it is a reaction to the window as it is right now, and restoring last
   * week's 700px into a window that has since been made small is a worse
   * opening state than the default.
   */
  artifactWidth: number;

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

  openArtifact: (a: Artifact) => void;
  /** Follow a still-streaming artifact, but only the one already on show. */
  refreshArtifact: (a: Artifact) => void;
  closeArtifact: () => void;
  setArtifactMode: (m: ArtifactMode) => void;
  setArtifactWidth: (px: number) => void;

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
  artifact: null,
  artifactMode: "preview",
  artifactWidth: ARTIFACT_W_DEFAULT,
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

  openArtifact: (artifact) =>
    set((s) => ({
      artifact,
      // A different artifact arrives as itself, not as whatever view the last
      // one was left in; the same one reopened keeps the view it was reading.
      artifactMode: s.artifact?.key === artifact.key ? s.artifactMode : "preview",
    })),
  refreshArtifact: (artifact) =>
    set((s) => (s.artifact?.key === artifact.key ? { artifact } : {})),
  closeArtifact: () => set({ artifact: null }),
  setArtifactMode: (artifactMode) => set({ artifactMode }),
  setArtifactWidth: (artifactWidth) => set({ artifactWidth }),

  notify: (text) => {
    noticeId += 1;
    set({ notice: { text, id: noticeId }, announcement: text });
  },
  announce: (announcement) => {
    if (get().announcement !== announcement) set({ announcement });
  },
}));
