/**
 * Navigation + workspaces + health (GUI §3.2).
 *
 * "Every degraded state is visible from the sidebar without navigating"
 * (GUI §11). `list_workspaces` now returns `unindexed` and `cloudOnly` per
 * workspace, so this can say *which* workspace is the problem instead of
 * showing one global number that names nobody — and it says it in words on the
 * row, not only as a tinted count you would have to open Status to decode.
 *
 * Clicking a workspace scopes the Files browser to it. That is the only thing
 * a workspace row can do that is not a lie: there is no per-workspace search
 * filter in the command surface.
 */

import { forwardRef } from "react";

import styles from "./Sidebar.module.css";
import { cx } from "../lib/cx";
import { count } from "../lib/format";
import { Icon, type IconName } from "./Icon";
import { useWorkspaces } from "../queries";
import { useUi, type View } from "../store";
import type { WorkspaceRow } from "../api";

const NAV: ReadonlyArray<{ view: View; label: string; icon: IconName }> = [
  { view: "search", label: "Search", icon: "search" },
  { view: "ask", label: "Ask", icon: "ask" },
  { view: "files", label: "Files", icon: "file" },
  { view: "models", label: "Models", icon: "ask" },
  { view: "status", label: "Status", icon: "activity" },
  { view: "settings", label: "Settings", icon: "settings" },
];

interface Degradation {
  degraded: boolean;
  word: string;
  /** Short phrases shown under the row. Empty when nothing is wrong. */
  issues: string[];
}

/**
 * What is wrong with this workspace, from the fields the command returns.
 *
 * Nothing here is inferred, and **`unindexed` is deliberately not one of these
 * states**. It counts every file with no searchable text, and on a real corpus
 * most of those are photos and binaries with no parser — which the spec calls
 * expected, not a failure. Warning about them made a folder of holiday photos
 * render as broken, and a warning that fires on the ordinary case is one people
 * learn to ignore. What is actually wrong is a parse that failed and a file no
 * ingest has reached; `cloudOnly > 0` means contents were deliberately not read
 * (TIER-008). Each is shown as a count, because a warning without a magnitude
 * cannot be triaged.
 */
function workspaceState(w: WorkspaceRow): Degradation {
  const issues: string[] = [];
  if (w.parseFailed > 0) issues.push(`${count(w.parseFailed)} could not be read`);
  if (w.notProcessed > 0) issues.push(`${count(w.notProcessed)} not read yet`);
  if (w.cloudOnly > 0) issues.push(`${count(w.cloudOnly)} cloud-only`);
  if (w.files === 0) {
    return { degraded: true, word: "nothing indexed", issues };
  }
  if (issues.length > 0) {
    return { degraded: true, word: issues.join(" · "), issues };
  }
  return { degraded: false, word: "live", issues };
}

export const Sidebar = forwardRef<HTMLElement>(function Sidebar(_props, ref) {
  const view = useUi((s) => s.view);
  const setView = useUi((s) => s.setView);
  const collapsed = useUi((s) => s.sidebarCollapsed);
  const focusPane = useUi((s) => s.focusPane);
  const filter = useUi((s) => s.workspaceFilter);
  const setFilter = useUi((s) => s.setWorkspaceFilter);

  const workspaces = useWorkspaces();
  const rows = workspaces.data ?? [];

  return (
    <nav
      ref={ref}
      tabIndex={-1}
      className={cx(styles.sidebar, collapsed && styles.collapsedRail)}
      aria-label="Sections and workspaces"
      aria-hidden={collapsed || undefined}
      onFocus={() => focusPane("sidebar")}
    >
      <ul className={styles.group}>
        {NAV.map((item) => (
          <li key={item.view}>
            <button
              type="button"
              className={cx(styles.item, view === item.view && styles.active)}
              aria-current={view === item.view ? "page" : undefined}
              onClick={() => setView(item.view)}
            >
              <Icon name={item.icon} size={14} className={styles.icon} />
              <span className={styles.label}>{item.label}</span>
            </button>
          </li>
        ))}
      </ul>

      <div className={styles.rule} />

      <h2 className={styles.heading}>Workspaces</h2>

      {workspaces.isError && (
        <p className={styles.problem}>
          <Icon name="warning" size={12} />
          <span>{workspaces.error.message}</span>
        </p>
      )}

      <ul className={cx(styles.group, styles.scroll)}>
        <li>
          <button
            type="button"
            className={cx(
              styles.wsRow,
              filter === null && view === "files" && styles.wsActive,
            )}
            onClick={() => {
              setFilter(null);
              setView("files");
            }}
          >
            <span />
            <span className={styles.label}>All workspaces</span>
            <span className={cx("mono", styles.metric)}>
              {count(rows.length)}
            </span>
          </button>
        </li>

        {rows.map((w) => {
          const state = workspaceState(w);
          const selected = filter === w.name && view === "files";
          return (
            <li key={w.name}>
              <button
                type="button"
                className={cx(
                  styles.wsRow,
                  selected && styles.wsActive,
                  state.issues.length > 0 && styles.wsRowDegraded,
                )}
                title={`${w.path} — ${state.word}`}
                onClick={() => {
                  setFilter(w.name);
                  setView("files");
                }}
              >
                {state.degraded ? (
                  <Icon name="warning" size={11} className={styles.warnIcon} />
                ) : (
                  <span className={styles.dot} aria-hidden="true" />
                )}
                <span className={styles.label}>{w.name}</span>
                <span className="srOnly">{state.word}</span>
                <span
                  className={cx(
                    "mono",
                    styles.metric,
                    w.files === 0 && styles.warnText,
                  )}
                >
                  {count(w.files)}
                </span>
                {state.issues.length > 0 && (
                  <span className={cx("mono", styles.wsIssues)}>
                    {state.issues.map((i) => (
                      <span key={i}>{i}</span>
                    ))}
                  </span>
                )}
              </button>
            </li>
          );
        })}

        {!workspaces.isError && rows.length === 0 && (
          <li className={styles.none}>No workspaces yet</li>
        )}
      </ul>

      <div className={styles.spacer} />
    </nav>
  );
});
