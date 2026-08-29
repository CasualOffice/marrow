/**
 * `?` — the keyboard map (GUI §5.1).
 *
 * It lists every binding, including the three that currently have no desktop
 * command behind them. A shortcut that exists but does nothing is a bug; a
 * shortcut that exists and says what is missing is a to-do list.
 */

import { useEffect, useRef } from "react";

import styles from "./ShortcutsDialog.module.css";
import { cx } from "../lib/cx";
import { Kbd } from "./Kbd";
import { Icon } from "./Icon";
import { useUi } from "../store";

interface Binding {
  keys: string;
  label?: string;
  action: string;
  /** Set when the binding has no command behind it yet. */
  missing?: string;
}

const GROUPS: ReadonlyArray<{ title: string; items: Binding[] }> = [
  {
    title: "Finding",
    items: [
      { keys: "⌘K", label: "Command K", action: "Quick-find overlay" },
      { keys: "⌘F", label: "Command F", action: "Focus the search field" },
      { keys: "Esc", action: "Clear the query · close the overlay" },
    ],
  },
  {
    title: "Moving",
    items: [
      { keys: "↓ / ↑", action: "Move the selection" },
      { keys: "j / k", action: "Move the selection (when the list has focus)" },
      { keys: "⌘1–9", label: "Command one to nine", action: "Jump to result n" },
      { keys: "Tab", action: "Cycle panes" },
      { keys: "⌘\\", label: "Command backslash", action: "Toggle the sidebar" },
    ],
  },
  {
    title: "Acting",
    items: [
      { keys: "↵", label: "Return", action: "Open the result in the detail pane" },
      { keys: "⌘C", label: "Command C", action: "Copy the citation (path:line)" },
      {
        keys: "⌘↵",
        label: "Command Return",
        action: "Open in $EDITOR at the line",
        missing: "needs open_in_editor",
      },
      {
        keys: "⇧↵",
        label: "Shift Return",
        action: "Reveal in Finder",
        missing: "needs reveal_in_file_manager",
      },
      { keys: "?", action: "This list" },
    ],
  },
];

export function ShortcutsDialog() {
  const open = useUi((s) => s.shortcutsOpen);
  const setOpen = useUi((s) => s.setShortcutsOpen);
  const closeRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (open) closeRef.current?.focus();
  }, [open]);

  if (!open) return null;

  return (
    <div
      className={styles.scrim}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) setOpen(false);
      }}
    >
      <div
        className={styles.panel}
        role="dialog"
        aria-modal="true"
        aria-labelledby="shortcuts-title"
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            setOpen(false);
          }
          // The only focusable thing inside is the close button, so keeping
          // focus here is a matter of not letting Tab leave.
          if (e.key === "Tab") e.preventDefault();
        }}
      >
        <header className={styles.head}>
          <h1 id="shortcuts-title" className={styles.title}>
            Keyboard
          </h1>
          <button
            ref={closeRef}
            type="button"
            className={styles.close}
            aria-label="Close"
            onClick={() => setOpen(false)}
          >
            <Icon name="close" size={14} />
          </button>
        </header>

        <div className={styles.body}>
          {GROUPS.map((g) => (
            <section key={g.title} className={styles.group}>
              <h2 className={styles.groupTitle}>{g.title}</h2>
              <dl className={styles.list}>
                {g.items.map((b) => (
                  <div key={b.keys + b.action} className={styles.rowItem}>
                    <dt className={styles.keys}>
                      <Kbd {...(b.label === undefined ? {} : { label: b.label })}>
                        {b.keys}
                      </Kbd>
                    </dt>
                    <dd
                      className={cx(
                        styles.action,
                        b.missing !== undefined && styles.pending,
                      )}
                    >
                      {b.action}
                      {b.missing !== undefined && (
                        <span className={cx("mono", styles.missing)}>
                          {b.missing}
                        </span>
                      )}
                    </dd>
                  </div>
                ))}
              </dl>
            </section>
          ))}
        </div>
      </div>
    </div>
  );
}
