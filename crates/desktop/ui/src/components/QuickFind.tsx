/**
 * Quick find — the 80% case: "where is that thing" (GUI §3.1).
 *
 * No chrome, no title bar, centred, ~640px. Results appear as you type, lexical
 * first, never a spinner. Dismisses on Escape or blur and remembers nothing: a
 * quick-find that persists state becomes a window you have to manage, so
 * closing it clears the query.
 *
 * TODO: GUI §5.1 binds this to `⌥Space` as a *global* hotkey that works when
 * the app is not focused. That needs `tauri-plugin-global-shortcut` registered
 * in `crates/desktop/src/main.rs` and a matching entry in
 * `crates/desktop/capabilities/main.json`, plus a second borderless window —
 * all Rust, none of it reachable from here. Until then the overlay is bound to
 * `⌘K` inside the app and rendered in-window rather than as its own window.
 */

import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";

import styles from "./QuickFind.module.css";
import { cx } from "../lib/cx";
import { age, baseOf, count, dirOf, ms } from "../lib/format";
import { Icon } from "./Icon";
import { Kbd, KeyHint } from "./Kbd";
import { ProvenanceBadge, ReasonBadge, SelfBadge } from "./Badges";
import { useDebounced, useSearch } from "../queries";
import { anchorOf, hitKey, useUi } from "../store";
import { copyCitation, unavailable } from "../actions";
import type { SearchHit } from "../api";

/**
 * How many results the lens shows. Five fits the default window without a
 * scroll, which is the point of a lens: if you are scrolling it, the main
 * window is the better tool and `⌘↵` takes you there.
 */
const VISIBLE = 5;

export function QuickFind() {
  const open = useUi((s) => s.quickFindOpen);
  const query = useUi((s) => s.quickFindQuery);
  const setQuery = useUi((s) => s.setQuickFindQuery);
  const close = useUi((s) => s.closeQuickFind);
  const setAnchor = useUi((s) => s.setAnchor);
  const setView = useUi((s) => s.setView);

  const inputRef = useRef<HTMLInputElement>(null);

  const debounced = useDebounced(query);
  const q = useSearch(debounced, VISIBLE);
  const hits = useMemo(() => (q.data?.hits ?? []).slice(0, VISIBLE), [q.data]);

  // Anchored to identity here too, so the highlighted row does not jump to a
  // different file when the ranking settles under the cursor.
  const [cursorKey, setCursorKey] = useState<string | null>(null);
  const found = hits.findIndex((h) => hitKey(h) === cursorKey);
  const index = found >= 0 ? found : 0;

  useEffect(() => {
    if (open) {
      inputRef.current?.focus();
      setCursorKey(null);
    }
  }, [open]);

  if (!open) return null;

  const openInMarrow = (hit: SearchHit) => {
    setView("search");
    setAnchor(anchorOf(hit));
    useUi.getState().setQuery(query);
    close();
  };

  const move = (delta: number) => {
    if (hits.length === 0) return;
    const next = Math.min(Math.max(index + delta, 0), hits.length - 1);
    const h = hits[next];
    if (h) setCursorKey(hitKey(h));
  };

  const onKeyDown = (e: ReactKeyboardEvent) => {
    const meta = e.metaKey || e.ctrlKey;

    if (e.key === "Escape") {
      e.preventDefault();
      close();
      return;
    }
    if (e.key === "ArrowDown" || (meta && e.key === "n")) {
      e.preventDefault();
      move(1);
      return;
    }
    if (e.key === "ArrowUp" || (meta && e.key === "p")) {
      e.preventDefault();
      move(-1);
      return;
    }
    if (meta && /^[1-9]$/.test(e.key)) {
      e.preventDefault();
      const hit = hits[Number(e.key) - 1];
      if (hit) openInMarrow(hit);
      return;
    }
    if (meta && e.key.toLowerCase() === "c") {
      const hit = hits[index];
      if (hit) {
        e.preventDefault();
        void copyCitation(anchorOf(hit));
      }
      return;
    }
    if (e.key === "Enter") {
      e.preventDefault();
      const hit = hits[index];
      if (!hit) return;
      if (e.shiftKey) unavailable("reveal");
      else openInMarrow(hit);
      return;
    }
    // Focus stays inside the overlay: it is the only thing on screen.
    if (e.key === "Tab") e.preventDefault();
  };

  return (
    <div
      className={styles.scrim}
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) close();
      }}
    >
      <div
        className={styles.panel}
        role="dialog"
        aria-modal="true"
        aria-label="Quick find"
        onKeyDown={onKeyDown}
      >
        <div className={styles.queryRow}>
          <Icon name="search" size={17} className={styles.queryIcon} />
          <input
            ref={inputRef}
            className={styles.input}
            type="text"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Find anything"
            aria-label="Quick find"
            autoComplete="off"
            spellCheck={false}
          />
        </div>

        {hits.length > 0 && (
          <ul className={styles.hits} role="listbox" aria-label="Results">
            {hits.map((h, i) => {
              const selected = i === index;
              return (
                <li
                  key={hitKey(h)}
                  role="option"
                  aria-selected={selected}
                  className={cx(styles.hit, selected && styles.selected)}
                  onMouseDown={(e) => {
                    e.preventDefault();
                    openInMarrow(h);
                  }}
                >
                  <Kbd className={styles.jump}>{`⌘${i + 1}`}</Kbd>
                  <div className={styles.hitBody}>
                    <div className={styles.hitHead}>
                      <span className={styles.hitName}>
                        {baseOf(h.relativePath)}
                        {h.line === null ? "" : `:${h.line}`}
                      </span>
                      <span className={styles.grow} />
                      <ReasonBadge reason={h.reason} />
                      <ProvenanceBadge provenance={h.provenance} />
                      <SelfBadge citable={h.citable} />
                      <span className={styles.age}>{age(h.modifiedMs)}</span>
                    </div>
                    <div className={cx("mono", styles.hitDir)}>
                      {dirOf(h.relativePath) || "/"}
                    </div>
                    <div className={cx("mono", styles.hitSnippet)}>
                      {h.excerpt.split("\n").find((l) => l.trim() !== "") ?? ""}
                    </div>
                  </div>
                </li>
              );
            })}
          </ul>
        )}

        <div className={styles.footer}>
          <KeyHint keys="↵" label="Return" action="open in Marrow" />
          <KeyHint
            keys={`⌘1–${Math.max(1, Math.min(hits.length, 9))}`}
            label="Command number"
            action="jump"
          />
          <KeyHint keys="⌘C" label="Command C" action="cite" />
          <span className={styles.grow} />
          <span className="mono">
            {q.data === undefined
              ? "—"
              : `${count(q.data.matched)} · ${ms(q.data.elapsedMs)}`}
          </span>
        </div>
      </div>
    </div>
  );
}
