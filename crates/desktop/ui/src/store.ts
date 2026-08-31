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
 * The sections, in the order the switcher shows them and the order `⌘⌥n`
 * counts them.
 *
 * One list, so "⌘⌥3 is the third button" cannot quietly stop being true by
 * someone reordering the switcher.
 */
export const VIEWS: readonly View[] = [
  "search",
  "ask",
  "files",
  "models",
  "status",
  "settings",
];

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
  /** Folded to its rail. Persisted: it was being refolded on every launch. */
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

  /**
   * Which stored conversation Ask is showing. `null` is a thread that has not
   * been saved yet, which is every thread until its first answer finishes.
   *
   * This is window state and not core state: it is *which* conversation is on
   * screen, not what is in it. The turns themselves come from `load_conversation`.
   */
  activeConversationId: string | null;
  /**
   * Bumped whenever the thread on screen must be replaced — a different
   * conversation opened, or a new one started.
   *
   * A counter rather than a flag because "open the conversation I am already
   * looking at" is a real request (it discards an unsaved draft and reloads),
   * and an id alone cannot express it. Ask watches this, not the id, so
   * recording the id of a thread it has just saved does not reload the thread.
   */
  conversationEpoch: number;

  quickFindOpen: boolean;
  quickFindQuery: string;
  shortcutsOpen: boolean;

  /**
   * Whether the guided setup is on screen.
   *
   * Three states, and the third is the point:
   *
   *   `true`  — opened deliberately, from the button on Status.
   *   `false` — closed deliberately, for this run of the app.
   *   `null`  — nobody has said. **Decide from real state**: the app opens the
   *             flow when there are no workspaces at all, which is the one
   *             situation where the window is a search box over nothing.
   *
   * There is deliberately no persisted "has seen it" flag. A flag is a claim
   * about the user, and it is wrong the moment they delete everything and are
   * back to an empty window with no way forward. Asking the index instead means
   * the help comes back exactly when it is needed again, and never appears for
   * someone who is already working.
   */
  setupOpen: boolean | null;

  /**
   * Files are being dragged over the window, and how many.
   *
   * Window state and nothing else: the *paths* the drag carries stay in Rust.
   * Tauri forwards them here as well and this store deliberately keeps only the
   * count, so there is nothing for a later edit to be tempted to send back.
   */
  dragging: { over: boolean; count: number };
  /** A drop is being copied and indexed. Cleared by the result event. */
  dropBusy: boolean;

  /**
   * Thorough rather than Fast, remembered across launches.
   *
   * It used to live in `AskView`'s component state, so a user who wanted
   * thorough answers re-chose it on every launch and a mode switch that resets
   * itself is indistinguishable from one that does not work. Persisted here
   * beside the theme because it is the same kind of thing — a window-local
   * preference, not index state.
   */
  thorough: boolean;

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

  /** Show a stored conversation in Ask. Switches the view to it. */
  openConversation: (id: string) => void;
  /** Start an empty thread. Nothing is written until it has an answer in it. */
  startConversation: () => void;
  /** Ask reporting which conversation it is in, without asking for a reload. */
  setActiveConversationId: (id: string | null) => void;

  openQuickFind: () => void;
  closeQuickFind: () => void;
  setQuickFindQuery: (q: string) => void;
  setShortcutsOpen: (open: boolean) => void;
  setSetupOpen: (open: boolean) => void;
  setDragging: (d: { over: boolean; count: number }) => void;
  setDropBusy: (busy: boolean) => void;
  setThorough: (thorough: boolean) => void;

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

/**
 * Window preferences that outlive the process, in `localStorage`.
 *
 * Same store and the same reasoning as the theme in `theme.ts`: each of these
 * is local to this window, is not part of the index, and does not travel with
 * it. **A choice that silently reverts is worse than one that is not offered** —
 * the user cannot tell it apart from a control that does nothing, and both of
 * these were re-made on every launch.
 *
 * A `localStorage` that throws — private mode, a disabled origin — costs the
 * preference and nothing else, so both directions swallow it.
 *
 * Deliberately not here: `artifactWidth`, which is a reaction to the window as
 * it is right now (restoring last week's 700px into a window since made small
 * is a worse opening state than the default), and `workspaceFilter`, which is
 * navigation rather than preference.
 */
function loadFlag(key: string): boolean {
  try {
    return window.localStorage.getItem(key) === "true";
  } catch {
    return false;
  }
}

function saveFlag(key: string, on: boolean): void {
  try {
    window.localStorage.setItem(key, String(on));
  } catch {
    /* it still applies for this session */
  }
}

/** Thorough rather than Fast. */
const THOROUGH_KEY = "marrow.thorough";
/** The sidebar folded away to its rail. */
const COLLAPSED_KEY = "marrow.sidebarCollapsed";

/**
 * The panes each view actually mounts, in Tab order.
 *
 * This used to be one fixed three-pane list for the whole app, and only Search
 * mounts three. Files attaches `detailRef` and nothing else; Models, Status,
 * Settings and Ask attach none. So the cycle stepped onto refs that were
 * `null` — and because `App` swallowed Tab to run the cycle, **no control on
 * those pages could be reached by keyboard at all**, the API-key field
 * included.
 *
 * A view with fewer than two panes therefore has no cycle, and `App` leaves Tab
 * to the browser there. Native tab order is the thing a browser gives away for
 * free; taking it away and putting nothing in its place is how a whole page
 * goes dark.
 */
const VIEW_PANES: Record<View, readonly Pane[]> = {
  search: ["sidebar", "results", "detail"],
  files: ["sidebar", "detail"],
  ask: [],
  models: [],
  status: [],
  settings: [],
};

/**
 * The panes `view` cycles right now. The sidebar drops out when it is
 * collapsed — there is nothing to focus behind a rail.
 *
 * Fewer than two means the cycle is not a cycle, and Tab belongs to the
 * browser. `App` asks this the same way `cyclePane` does, so the key and the
 * action can never disagree about which views have panes.
 */
export function panesFor(
  view: View,
  sidebarCollapsed: boolean,
): readonly Pane[] {
  const panes = VIEW_PANES[view];
  return sidebarCollapsed ? panes.filter((p) => p !== "sidebar") : panes;
}

export const useUi = create<UiState>((set, get) => ({
  view: "search",
  query: "",
  workspaceFilter: null,
  theme: loadTheme(),
  sidebarCollapsed: loadFlag(COLLAPSED_KEY),
  focusedPane: "results",
  anchor: null,
  activeConversationId: null,
  conversationEpoch: 0,
  quickFindOpen: false,
  quickFindQuery: "",
  shortcutsOpen: false,
  setupOpen: null,
  dragging: { over: false, count: 0 },
  dropBusy: false,
  thorough: loadFlag(THOROUGH_KEY),
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
    set((s) => {
      const sidebarCollapsed = !s.sidebarCollapsed;
      // Remembered, like the theme: someone who works with the rail folded away
      // was refolding it every launch.
      saveFlag(COLLAPSED_KEY, sidebarCollapsed);
      return {
        sidebarCollapsed,
        focusedPane:
          sidebarCollapsed && s.focusedPane === "sidebar"
            ? "results"
            : s.focusedPane,
      };
    }),
  focusPane: (focusedPane) => set({ focusedPane }),
  cyclePane: (back) =>
    set((s) => {
      const panes = panesFor(s.view, s.sidebarCollapsed);
      // Nothing to cycle: leave `focusedPane` alone rather than parking it on a
      // pane this view does not mount. The old code moved it anyway, so the
      // effect that follows `focusedPane` then focused `null`.
      if (panes.length === 0) return {};
      // `indexOf` is -1 when the focused pane is not one of this view's, which
      // lands on the first pane. That is the right answer for arriving from a
      // view with a different set.
      const i = panes.indexOf(s.focusedPane);
      const next = panes[(i + (back ? -1 : 1) + panes.length) % panes.length];
      return { focusedPane: next ?? panes[0] ?? "results" };
    }),
  setAnchor: (anchor) => set({ anchor }),

  openConversation: (id) =>
    set((s) => ({
      view: "ask",
      activeConversationId: id,
      conversationEpoch: s.conversationEpoch + 1,
    })),
  startConversation: () =>
    set((s) => ({
      view: "ask",
      activeConversationId: null,
      conversationEpoch: s.conversationEpoch + 1,
    })),
  setActiveConversationId: (activeConversationId) => set({ activeConversationId }),

  openQuickFind: () => set({ quickFindOpen: true }),
  closeQuickFind: () => set({ quickFindOpen: false, quickFindQuery: "" }),
  setQuickFindQuery: (quickFindQuery) => set({ quickFindQuery }),
  setShortcutsOpen: (shortcutsOpen) => set({ shortcutsOpen }),
  setSetupOpen: (setupOpen) => set({ setupOpen }),
  setDragging: (dragging) => set({ dragging }),
  setDropBusy: (dropBusy) => set({ dropBusy }),
  setThorough: (thorough) => {
    saveFlag(THOROUGH_KEY, thorough);
    set({ thorough });
  },

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
