/**
 * Two jobs, both quiet.
 *
 * The live region announces result counts and state changes to a screen reader
 * (GUI §8). The strip shows the same text on screen for a few seconds — it is
 * how `⌘C` confirms a citation was copied and how a key with no command behind
 * it explains itself. Never a modal, never a spinner.
 */

import { useEffect, useState } from "react";

import styles from "./Notice.module.css";
import { Icon } from "./Icon";
import { useUi } from "../store";

const DWELL_MS = 5000;

export function Notice() {
  const notice = useUi((s) => s.notice);
  const announcement = useUi((s) => s.announcement);
  const [shown, setShown] = useState<string | null>(null);

  useEffect(() => {
    if (!notice) return;
    setShown(notice.text);
    const t = window.setTimeout(() => setShown(null), DWELL_MS);
    return () => window.clearTimeout(t);
  }, [notice]);

  return (
    <>
      <div className="srOnly" role="status" aria-live="polite" aria-atomic="true">
        {announcement}
      </div>
      {shown !== null && (
        <div className={styles.strip}>
          <span className={styles.text}>{shown}</span>
          <button
            type="button"
            className={styles.dismiss}
            aria-label="Dismiss"
            onClick={() => setShown(null)}
          >
            <Icon name="close" size={12} />
          </button>
        </div>
      )}
    </>
  );
}
