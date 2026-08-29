/**
 * Navigation + workspaces + health (GUI §3.2).
 *
 * "Every degraded state is visible from the sidebar without navigating"
 * (GUI §11) — so the workspace rows carry live state, and the index summary
 * below them shows the cloud-only count whether or not it is zero (TIER-008: a
 * silent zero is indistinguishable from "no cloud files").
 */

import { forwardRef } from "react";

import styles from "./Sidebar.module.css";
import { cx } from "../lib/cx";
import { count } from "../lib/format";
import { Icon, type IconName } from "./Icon";
import { useIndexHealth, useWorkspaces } from "../queries";
import { useUi, type View } from "../store";
import type { WorkspaceRow } from "../api";

const NAV: ReadonlyArray<{ view: View; label: string; icon: IconName }> = [
  { view: "search", label: "Search", icon: "search" },
  { view: "files", label: "Files", icon: "file" },
  { view: "status", label: "Status", icon: "activity" },
];

type Tone = "ok" | "warn" | "error";

/**
 * The only per-workspace signal the command surface exposes is the active file
 * count, so that is what the dot means, and the row says the word too. Nothing
 * here is inferred: an unreachable workspace list is an error, not a guess.
 */
function workspaceState(w: WorkspaceRow): { tone: Tone; word: string } {
  if (w.files === 0) return { tone: "warn", word: "nothing indexed" };
  return { tone: "ok", word: "live" };
}

export const Sidebar = forwardRef<HTMLElement>(function Sidebar(_props, ref) {
  const view = useUi((s) => s.view);
  const setView = useUi((s) => s.setView);
  const collapsed = useUi((s) => s.sidebarCollapsed);
  const focusPane = useUi((s) => s.focusPane);
  const notify = useUi((s) => s.notify);

  const workspaces = useWorkspaces();
  const health = useIndexHealth();

  const rows = workspaces.data ?? [];
  const cloudOnly = health.data?.cloudOnly;

  return (
    <nav
      ref={ref}
      tabIndex={-1}
      className={cx(styles.sidebar, collapsed && styles.collapsed)}
      aria-label="Sections and workspaces"
      onFocus={() => focusPane("sidebar")}
    >
      <ul className={styles.group}>
        {NAV.map((item) => (
          <li key={item.view}>
            <button
              type="button"
              className={cx(styles.item, view === item.view && styles.active)}
              aria-current={view === item.view ? "page" : undefined}
              title={collapsed ? item.label : undefined}
              onClick={() => setView(item.view)}
            >
              <Icon name={item.icon} />
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

      <ul className={styles.group}>
        {rows.map((w) => {
          const state = workspaceState(w);
          return (
            <li key={w.name}>
              <button
                type="button"
                className={styles.item}
                title={`${w.path} — ${state.word}`}
                onClick={() => {
                  setView("files");
                  notify(`${w.name}: ${count(w.files)} files indexed`);
                }}
              >
                {state.tone === "ok" ? (
                  <span className={cx(styles.dot, styles.ok)} aria-hidden="true" />
                ) : (
                  <Icon name="warning" size={11} className={styles.warnIcon} />
                )}
                <span className={styles.label}>{w.name}</span>
                <span className="srOnly">{state.word}</span>
                <span
                  className={cx(
                    "mono",
                    styles.metric,
                    state.tone === "warn" && styles.warnText,
                  )}
                >
                  {count(w.files)}
                </span>
              </button>
            </li>
          );
        })}
        {!workspaces.isError && rows.length === 0 && (
          <li className={styles.none}>No workspaces yet</li>
        )}
      </ul>

      <div className={styles.spacer} />

      {/* Degraded index state, visible without navigating to Status. */}
      {cloudOnly !== undefined && cloudOnly > 0 && (
        <button
          type="button"
          className={cx(styles.item, styles.degraded)}
          onClick={() => setView("status")}
          title="Cloud-only files were not read. Open Status."
        >
          <Icon name="warning" size={12} />
          <span className={styles.label}>cloud-only</span>
          <span className={cx("mono", styles.metric, styles.warnText)}>
            {count(cloudOnly)}
          </span>
        </button>
      )}

      <ul className={styles.group}>
        <li>
          <button
            type="button"
            className={styles.item}
            title="Settings"
            onClick={() =>
              notify(
                "Settings has no desktop command yet: M1 exposes five read-only commands and no configuration surface.",
              )
            }
          >
            <Icon name="settings" />
            <span className={styles.label}>Settings</span>
          </button>
        </li>
      </ul>
    </nav>
  );
});
