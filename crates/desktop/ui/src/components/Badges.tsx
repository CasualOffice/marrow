/**
 * Provenance is always on screen (GUI §5.4).
 *
 * A11Y-003, non-negotiable: colour never carries meaning alone. Every badge
 * here is a *word* that happens to be tinted. Remove the colour and the row
 * still says `exact`, `~approx`, `[self]`.
 */

import styles from "./Badges.module.css";
import { cx } from "../lib/cx";
import { Icon } from "./Icon";

/* ── match reason ────────────────────────────────────────────────────────── */

const REASON_CLASS: Record<string, string> = {
  exact: styles.exact ?? "",
  semantic: styles.semantic ?? "",
  path: styles.path ?? "",
  recent: styles.recent ?? "",
};

/** Why it matched — UX principle 3. Without it, hybrid ranking is a black box. */
export function ReasonBadge({ reason }: { reason: string }) {
  return (
    <span className={cx(styles.reason, REASON_CLASS[reason] ?? styles.path)}>
      {reason}
    </span>
  );
}

/* ── provenance ──────────────────────────────────────────────────────────── */

/** What each non-exact class actually means, spelled out for the tooltip. */
const PROVENANCE_MEANING: Record<string, string> = {
  degraded:
    "Degraded provenance: the text was recovered, but the exact source span is not guaranteed.",
  approximate:
    "Approximate provenance: this location is a best estimate, not an exact span.",
  metadata_only:
    "Metadata only: the contents were never read, so nothing here cites the text.",
};

/**
 * `~approx` on anything not `exact` (GUI §5.4, UX principle 6). Silent
 * precision loss is the one thing that would destroy the product's premise, so
 * this renders for every unknown class too, not just the three we know.
 */
export function ProvenanceBadge({ provenance }: { provenance: string }) {
  if (provenance === "exact") return null;
  const meaning =
    PROVENANCE_MEANING[provenance] ??
    `Provenance is "${provenance}", which is not exact.`;
  return (
    <span className={cx(styles.badge, styles.approx)} title={meaning}>
      <span aria-hidden="true">~approx</span>
      <span className="srOnly">{meaning}</span>
    </span>
  );
}

/* ── origin ──────────────────────────────────────────────────────────────── */

const SELF_MEANING =
  "Written by an agent, not by you. It cannot be cited as a source.";

/** Invariant #13. `citable === false` means the agent wrote it. */
export function SelfBadge({ citable }: { citable: boolean }) {
  if (citable) return null;
  return (
    <span className={cx(styles.badge, styles.self)} title={SELF_MEANING}>
      <span aria-hidden="true">[self]</span>
      <span className="srOnly">{SELF_MEANING}</span>
    </span>
  );
}

/* ── workspace / index state ─────────────────────────────────────────────── */

export type StateTone = "ok" | "warn" | "error";

/**
 * A state pill. The dot is decorative; the word is the meaning. A degraded
 * watcher has to be legible in greyscale from across the room.
 */
export function StateBadge({
  tone,
  children,
}: {
  tone: StateTone;
  children: string;
}) {
  return (
    <span className={cx(styles.state, styles[tone])}>
      {tone === "ok" ? (
        <span className={styles.dot} aria-hidden="true" />
      ) : (
        <Icon name="warning" size={11} aria-hidden="true" />
      )}
      {children}
    </span>
  );
}
