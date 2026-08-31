import { useEffect, useState } from "react";

import styles from "./ConversationFinder.module.css";
import { Icon } from "./Icon";
import { age } from "../lib/format";
import { asUiError, searchConversations, type ConversationMatch } from "../api";
import { useUi } from "../store";

/**
 * Search the conversation list — by title, and by what was actually said.
 *
 * **Lives beside the list it searches.** It was rendered inside `AskView`,
 * above the thread, so a control for choosing *between* conversations took the
 * top of the column you read the current one in. That was never the design; it
 * was there because the change that added it could not touch the sidebar.
 *
 * The Rust side searches `conversations` and `conversation_turns` together and
 * returns the earliest matching turn with a snippet, so a hit deep in a thread
 * can say which turn it was rather than repeating the title.
 */
export function ConversationFinder() {
  const openConversation = useUi((s) => s.openConversation);
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [rows, setRows] = useState<readonly ConversationMatch[]>([]);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    let live = true;
    // Debounced only once there is something to debounce. The empty query is
    // the recent list and wants to be there the moment the field is focused;
    // a keystroke is worth the wait, because the results replace each other
    // and a stale one arriving late would overwrite a newer one.
    const timer = window.setTimeout(
      () => {
        searchConversations(query, 40)
          .then((found) => {
            if (!live) return;
            setRows(found);
            setError(null);
          })
          .catch((e) => {
            if (!live) return;
            setRows([]);
            setError(asUiError(e).message);
          });
      },
      query.trim() === "" ? 0 : 120,
    );
    return () => {
      live = false;
      window.clearTimeout(timer);
    };
  }, [open, query]);

  const choose = (id: string) => {
    setOpen(false);
    setQuery("");
    openConversation(id);
  };

  return (
    <div
      className={styles.finder}
      // Closed when focus leaves the whole control, not when the field alone
      // blurs: clicking a result moves focus to the button inside the panel,
      // and closing on that would unmount the row mid-click.
      onBlur={(e) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setOpen(false);
      }}
    >
      <div className={styles.box}>
        <Icon name="search" size={13} className={styles.icon} />
        <input
          type="text"
          className={styles.field}
          value={query}
          placeholder="Search conversations…"
          aria-label="Search conversations by title or by what was said"
          onFocus={() => setOpen(true)}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape") {
              e.preventDefault();
              // The window binds Escape as well, where it clears the *file*
              // search and takes focus to that field. Here it means this box.
              e.stopPropagation();
              if (query !== "") setQuery("");
              else {
                setOpen(false);
                e.currentTarget.blur();
              }
            }
          }}
        />
      </div>

      {open && (
        <div
          className={styles.panel}
          // WebKit does not focus a button when it is clicked, so the pointer
          // press would blur the field, close this panel and destroy the row
          // before the click ever landed on it. Keeping focus where it is makes
          // the click arrive. Tabbing to a row still moves focus, and the
          // `onBlur` above still closes on that.
          onMouseDown={(e) => e.preventDefault()}
        >
          {error !== null ? (
            <p className={styles.note}>{error}</p>
          ) : rows.length === 0 ? (
            <p className={styles.note}>
              {query.trim() === ""
                ? "No conversations yet. The first answer you keep starts one."
                : `Nothing in your conversations says “${query.trim()}”. Titles, questions and answers are all searched.`}
            </p>
          ) : (
            <ul className={styles.list}>
              {rows.map((c) => (
                <li key={c.id}>
                  <button
                    type="button"
                    className={styles.row}
                    onClick={() => choose(c.id)}
                  >
                    <span className={styles.title}>{c.title}</span>
                    <span className={styles.meta}>
                      {age(c.updatedMs)} · {c.turns} {c.turns === 1 ? "turn" : "turns"}
                      {/* Which turn matched, because "somewhere in nine" and
                          "in the first thing you asked" are different answers
                          to "is this the one?". */}
                      {c.matchedTurn !== null && ` · matched in turn ${c.matchedTurn}`}
                    </span>
                    {c.snippet !== null && (
                      <span className={styles.snippet}>{c.snippet}</span>
                    )}
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
      )}
    </div>
  );
}
