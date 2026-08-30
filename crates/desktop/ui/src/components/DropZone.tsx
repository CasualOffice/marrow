/**
 * Drop files onto the window.
 *
 * **There was no way to add a *file*.** Only whole folders, only through a
 * picker, so someone with a PDF on their desktop and a question about it had to
 * do the filing before Marrow would do the reading.
 *
 * This component is the window's half of it, and it is deliberately the smaller
 * half. The paths a drop carries are delivered by the operating system to
 * *Rust*, which copies and indexes them; Tauri forwards the same list here as
 * well, and everything below uses it for one thing only — counting files to
 * draw a number in an overlay. Nothing is ever sent back, because there is no
 * command that would take it: `add_files` opens a native panel and accepts no
 * path at all. The window cannot ask Marrow to read a file it invented.
 *
 * Tauri's own window drag-drop event is what makes this possible without
 * granting the WebView a filesystem capability, which would undo SEC-012 for
 * the sake of a convenience.
 */

import { useEffect } from "react";

import styles from "./DropZone.module.css";
import { cx } from "../lib/cx";
import { Icon } from "./Icon";
import { Kbd } from "./Kbd";
import { onDragAndDrop } from "../api";
import { describeDrop, refreshAfterIndexChange } from "../actions";
import { useUi } from "../store";

export function DropZone() {
  const dragging = useUi((s) => s.dragging);
  const busy = useUi((s) => s.dropBusy);

  useEffect(() => {
    let stop: (() => void) | null = null;
    let cancelled = false;

    void onDragAndDrop({
      onDrag: (d) => useUi.getState().setDragging(d),
      // The hover overlay comes down and the working state takes over until
      // Rust says what happened. Without it the window looks like it ignored
      // the drop for however long the copy and the parse take.
      onDropStarted: () => useUi.getState().setDropBusy(true),
      onDrop: (outcome) => {
        const ui = useUi.getState();
        ui.setDropBusy(false);
        if (outcome.error) {
          ui.notify(outcome.error.message);
          return;
        }
        if (outcome.report) {
          ui.notify(describeDrop(outcome.report));
          // The same refresh a picked file gets. A drop and a pick produce the
          // same change to the index and must not leave the window in two
          // different states.
          refreshAfterIndexChange();
        }
      },
    }).then((off) => {
      // Unsubscribing is asynchronous, and StrictMode mounts twice in dev.
      if (cancelled) off();
      else stop = off;
    });

    return () => {
      cancelled = true;
      stop?.();
      // A drag that was in progress when this unmounted has no listener left to
      // end it, and a stuck overlay would cover the whole window.
      useUi.getState().setDragging({ over: false, count: 0 });
    };
  }, []);

  const show = dragging.over || busy;
  if (!show) return null;

  return (
    <div className={cx(styles.scrim, busy && styles.working)} aria-hidden={!busy}>
      <div className={styles.panel} role="status" aria-live="polite">
        <Icon name={busy ? "activity" : "plus"} size={22} className={styles.glyph} />
        <p className={styles.title}>
          {busy
            ? "Reading them now"
            : dragging.count === 1
              ? "Drop it here"
              : `Drop ${dragging.count > 0 ? dragging.count : "them"} here`}
        </p>
        <p className={styles.detail}>
          {busy
            ? "Copying into Marrow's dropped-files folder and indexing. They become searchable as it finishes."
            : "They are copied into a folder Marrow owns, indexed straight away, and you can empty it whenever you like. Your originals are not moved."}
        </p>
        {!busy && (
          <p className={styles.alt}>
            <Kbd label="Command O">⌘O</Kbd> does the same thing from a panel
          </p>
        )}
      </div>
    </div>
  );
}
