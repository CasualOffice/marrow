/**
 * Conversations, then workspaces (GUI §3.2).
 *
 * The rail used to open with six section links and then a workspace list — an
 * IDE's shape, spending its one permanent column on destinations rather than on
 * content. The sections moved to the title strip; what is here now is the thing
 * you come back to. A conversation list is also what makes this column worth
 * its width at all: before it, the width held six links and a count.
 *
 * "Every degraded state is visible from the sidebar without navigating"
 * (GUI §11) still holds — the workspace rows below say which workspace is the
 * problem and in what way, in words on the row rather than as a tinted count.
 *
 * Every row has a keyboard path to everything the mouse can do to it: the two
 * icon buttons appear on hover *and* on focus, and `F2` / `⌫` do the same two
 * things from the row itself.
 */

import { forwardRef, useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";

import styles from "./Sidebar.module.css";
import { cx } from "../lib/cx";
import { age, count } from "../lib/format";
import { Icon } from "./Icon";
import { Kbd } from "./Kbd";
import { CONVERSATIONS_KEY, useConversations, useWorkspaces } from "../queries";
import { asUiError, deleteConversation, renameConversation } from "../api";
import { useUi } from "../store";
import type { ConversationSummary, WorkspaceRow } from "../api";

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
  const active = useUi((s) => s.activeConversationId);
  const openConversation = useUi((s) => s.openConversation);
  const startConversation = useUi((s) => s.startConversation);
  const notify = useUi((s) => s.notify);

  const client = useQueryClient();
  const conversations = useConversations();
  const threads = conversations.data ?? [];
  const workspaces = useWorkspaces();
  const rows = workspaces.data ?? [];

  /** The conversation being renamed in place, if any. */
  const [renaming, setRenaming] = useState<string | null>(null);

  const refresh = useCallback(
    () => client.invalidateQueries({ queryKey: CONVERSATIONS_KEY }),
    [client],
  );

  const rename = useCallback(
    async (id: string, title: string) => {
      setRenaming(null);
      try {
        await renameConversation(id, title);
        await refresh();
      } catch (e) {
        notify(asUiError(e).message);
      }
    },
    [notify, refresh],
  );

  const remove = useCallback(
    async (c: ConversationSummary) => {
      try {
        await deleteConversation(c.id);
        await refresh();
        // Leaving a deleted thread on screen would be showing something the
        // list says is gone. Its rows are still in the database — this is a
        // soft delete — but nothing in this window can reach them.
        if (active === c.id) startConversation();
        notify(`Deleted “${c.title}”.`);
      } catch (e) {
        notify(asUiError(e).message);
      }
    },
    [active, notify, refresh, startConversation],
  );

  return (
    <nav
      ref={ref}
      tabIndex={-1}
      className={cx(styles.sidebar, collapsed && styles.collapsedRail)}
      aria-label="Conversations and workspaces"
      aria-hidden={collapsed || undefined}
      onFocus={() => focusPane("sidebar")}
    >
      <div className={styles.top}>
        <button type="button" className={styles.newThread} onClick={startConversation}>
          <Icon name="plus" size={13} className={styles.icon} />
          <span className={styles.label}>New conversation</span>
          <Kbd label="Command N">⌘N</Kbd>
        </button>
      </div>

      <h2 className={styles.heading}>Conversations</h2>

      {conversations.isError && (
        <p className={styles.problem}>
          <Icon name="warning" size={12} />
          <span>{conversations.error.message}</span>
        </p>
      )}

      <ul className={cx(styles.group, styles.threads)}>
        {threads.map((c) => (
          <li key={c.id}>
            {renaming === c.id ? (
              <RenameField
                title={c.title}
                onCommit={(t) => void rename(c.id, t)}
                onCancel={() => setRenaming(null)}
              />
            ) : (
              <div
                className={cx(
                  styles.convRow,
                  active === c.id && view === "ask" && styles.convActive,
                )}
              >
                <button
                  type="button"
                  className={styles.convOpen}
                  title={`${c.title} — ${c.turns} ${c.turns === 1 ? "turn" : "turns"}`}
                  onClick={() => openConversation(c.id)}
                  onKeyDown={(e) => {
                    // The keyboard half of the two buttons beside it. Both are
                    // in the shortcuts dialog; neither is the only way in.
                    if (e.key === "F2") {
                      e.preventDefault();
                      setRenaming(c.id);
                    }
                    if (e.key === "Backspace" || e.key === "Delete") {
                      e.preventDefault();
                      void remove(c);
                    }
                  }}
                >
                  <span className={styles.label}>{c.title}</span>
                </button>
                <span className={styles.convMeta}>
                  <span className={cx("mono", styles.age)}>{age(c.updatedMs)}</span>
                  <span className={styles.rowActions}>
                    <button
                      type="button"
                      className={styles.rowAction}
                      aria-label={`Rename “${c.title}”`}
                      title="Rename (F2)"
                      onClick={() => setRenaming(c.id)}
                    >
                      <Icon name="pencil" size={11} />
                    </button>
                    <button
                      type="button"
                      className={styles.rowAction}
                      aria-label={`Delete “${c.title}”`}
                      title="Delete (⌫)"
                      onClick={() => void remove(c)}
                    >
                      <Icon name="trash" size={11} />
                    </button>
                  </span>
                </span>
              </div>
            )}
          </li>
        ))}

        {!conversations.isError && threads.length === 0 && (
          <li className={styles.none}>
            Nothing asked yet. A conversation is saved once it has an answer in
            it.
          </li>
        )}
      </ul>

      <div className={styles.rule} />

      <h2 className={styles.heading}>Workspaces</h2>

      {workspaces.isError && (
        <p className={styles.problem}>
          <Icon name="warning" size={12} />
          <span>{workspaces.error.message}</span>
        </p>
      )}

      <ul className={cx(styles.group, styles.spaces)}>
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
    </nav>
  );
});

/**
 * Renaming, in place.
 *
 * A field rather than a dialog: the name is one line and the row is where you
 * are looking. Enter commits, Escape abandons, and losing focus commits — the
 * three things a text field in a list is expected to do, and doing anything
 * else here would be a surprise with no upside.
 */
function RenameField({
  title,
  onCommit,
  onCancel,
}: {
  title: string;
  onCommit: (title: string) => void;
  onCancel: () => void;
}) {
  const [draft, setDraft] = useState(title);
  const field = useRef<HTMLInputElement>(null);
  /** Escape must not commit on the blur it causes on its way out. */
  const abandoned = useRef(false);

  useEffect(() => {
    field.current?.focus();
    field.current?.select();
  }, []);

  return (
    <input
      ref={field}
      className={styles.renameField}
      aria-label="Conversation name"
      value={draft}
      onChange={(e) => setDraft(e.target.value)}
      onBlur={() => {
        if (abandoned.current) return;
        if (draft.trim() === "" || draft === title) onCancel();
        else onCommit(draft);
      }}
      onKeyDown={(e) => {
        // Both keys are also bound on the window — Escape clears the search
        // query and takes focus with it. Inside this field they mean one thing
        // each, and a key that does two things at once does neither.
        if (e.key === "Enter" || e.key === "Escape") e.stopPropagation();
        if (e.key === "Enter") {
          e.preventDefault();
          if (draft.trim() === "" || draft === title) onCancel();
          else onCommit(draft);
        }
        if (e.key === "Escape") {
          e.preventDefault();
          abandoned.current = true;
          onCancel();
        }
      }}
    />
  );
}
