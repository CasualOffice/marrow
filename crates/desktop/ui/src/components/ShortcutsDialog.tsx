/**
 * `?` — the keyboard map (GUI §5.1).
 *
 * It lists every binding. `⌘↵` and `⇧↵` used to be on it as "needs a command
 * that does not exist"; `open_path` and `reveal_path` arrived and they are real
 * now. What is left marked is genuinely absent. A shortcut that exists but does
 * nothing is a bug; a shortcut that exists and says what is missing is a to-do
 * list.
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
    // Both are live handlers in `App.tsx`, and both are the *keyboard* half of
    // something that otherwise had only a mouse path — dropping files onto the
    // window is a gesture with no key at all, and "Add a folder" was a button
    // on the Status page and nowhere else.
    title: "Adding files",
    items: [
      {
        keys: "⌘O",
        label: "Command O",
        action: "Add files — copied in, indexed straight away",
      },
      {
        keys: "⇧⌘O",
        label: "Shift Command O",
        action: "Add a folder — indexed where it already is",
      },
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
    // The two row keys are live wherever a conversation row has focus, which is
    // the same place the pencil and the bin appear. Neither is the only way in:
    // GUI §11 asks for a keyboard equivalent to every mouse action, and a
    // shortcut that is the *sole* path is that rule read backwards.
    title: "Conversations",
    items: [
      { keys: "⌘N", label: "Command N", action: "Start a new conversation" },
      { keys: "F2", action: "Rename the focused conversation" },
      {
        keys: "⌫",
        label: "Delete",
        action: "Delete the focused conversation (it is kept, not erased)",
      },
    ],
  },
  {
    title: "Acting",
    items: [
      { keys: "↵", label: "Return", action: "Read it here — focus the preview" },
      {
        keys: "⌘↵",
        label: "Command Return",
        action: "Open in the system's default application",
      },
      { keys: "⇧↵", label: "Shift Return", action: "Reveal in the file manager" },
      { keys: "⌘C", label: "Command C", action: "Copy the citation (path:line)" },
      {
        keys: "⌥Space",
        label: "Option Space",
        action: "Quick find while another app has focus",
        missing: "needs a global-shortcut plugin",
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
