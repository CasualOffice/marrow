/**
 * Result anatomy — UX §4, and design/Main.dc.html.
 *
 * The order is the whole point:
 *   1. `relativePath:line`, bold, on its own line, first. Jumpable,
 *      copy-pasteable, and the eye scans a left-aligned column of paths far
 *      faster than paths embedded in prose.
 *   2. Match reason and age, right-aligned and dim, so they stay out of the way
 *      of that column.
 *   3. Up to two excerpt lines, mono, on a raised background. Enough to decide;
 *      more is a pager, not a search result.
 *   4. The breadcrumb, dimmed, last.
 */

import { memo } from "react";

import styles from "./ResultRow.module.css";
import { cx } from "../lib/cx";
import { age, dirOf, squeezeDir } from "../lib/format";
import { ProvenanceBadge, ReasonBadge, SelfBadge } from "./Badges";
import type { SearchHit } from "../api";

/**
 * Row height in px — the single source of truth. The virtualizer needs the
 * number and the CSS needs the length, so the CSS reads it back as a custom
 * property set on the scroller.
 *
 * Every row is the same height, so a re-rank moves rows without ever resizing
 * one: no measurement pass, no layout thrash (GUI §7). The composition is
 *
 *   26px  row padding (9+9) plus the excerpt box's own padding (4+4)
 *   72px  four text lines at the 16px root: head, two excerpt lines, breadcrumb
 *
 * and only the second part scales with the system text size, which is why it is
 * computed from the root font size rather than hard-coded (GUI §8).
 */
function measureRow(): number {
  const root =
    typeof document === "undefined"
      ? 16
      : parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
  return Math.round(28 + (72 * root) / 16);
}

export const ROW_HEIGHT = measureRow();

/** Two lines. "Enough to decide" (UX §4). */
function excerptLines(excerpt: string): string[] {
  const lines = excerpt.split("\n").filter((l) => l.trim() !== "");
  if (lines.length === 0) return [""];
  return lines.slice(0, 2);
}

export interface ResultRowProps {
  hit: SearchHit;
  selected: boolean;
  /** 1-based; shown as a `⌘n` cap for the first nine only. */
  ordinal: number;
  onSelect: (hit: SearchHit) => void;
  onOpen: (hit: SearchHit) => void;
}

export const ResultRow = memo(function ResultRow({
  hit,
  selected,
  ordinal,
  onSelect,
  onOpen,
}: ResultRowProps) {
  const dir = dirOf(hit.relativePath);
  const base = hit.relativePath.slice(dir.length);
  const shownDir = squeezeDir(dir);
  const suffix = hit.line === null ? "" : `:${hit.line}`;
  const lines = excerptLines(hit.excerpt);

  return (
    <div
      role="option"
      aria-selected={selected}
      id={`result-${hit.fileId}-${hit.line ?? "x"}`}
      className={cx(styles.row, selected && styles.selected)}
      onMouseDown={(e) => {
        // mousedown, not click: selecting a result must not cost a frame, and
        // it must not steal focus from the search field.
        e.preventDefault();
        onSelect(hit);
      }}
      onDoubleClick={() => onOpen(hit)}
    >
      {/* 1 — the citation, first and bold. */}
      <div className={styles.head}>
        <span className={styles.location} title={hit.location}>
          {dir !== "" && <span className={styles.dir}>{shownDir}</span>}
          <span className={styles.base}>{base}</span>
          {suffix !== "" && <span className={styles.lineNo}>{suffix}</span>}
        </span>

        {/* 2 — reason, provenance, origin, age. Right-aligned, dim. */}
        <span className={styles.meta}>
          <ReasonBadge reason={hit.reason} />
          <ProvenanceBadge provenance={hit.provenance} />
          <SelfBadge citable={hit.citable} />
          <span className={styles.age}>{age(hit.modifiedMs)}</span>
          {ordinal <= 9 && (
            <kbd className={cx("mono", styles.jump)} aria-hidden="true">
              ⌘{ordinal}
            </kbd>
          )}
        </span>
      </div>

      {/* 3 — the matched content. */}
      <div className={cx("mono", styles.excerpt)}>
        {lines.map((l, i) => (
          <div key={i} className={styles.excerptLine}>
            {l}
          </div>
        ))}
      </div>

      {/* 4 — the structural context prefix the chunker already computed. */}
      <div className={styles.crumb} title={hit.breadcrumb}>
        {hit.breadcrumb}
      </div>
    </div>
  );
});
