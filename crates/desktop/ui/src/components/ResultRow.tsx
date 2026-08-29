/**
 * Result anatomy — UX §4.
 *
 * The order is the whole point:
 *   1. `relativePath:line`, first, on its own line. Jumpable, copy-pasteable,
 *      and the eye scans a left-aligned column of paths far faster than paths
 *      embedded in prose.
 *   2. Match reason and age, right-aligned and dim, so they stay out of the way
 *      of that column.
 *   3. Up to two excerpt lines, mono, on a sunken plate. Enough to decide;
 *      more is a pager, not a search result.
 *   4. The breadcrumb, dimmed, last.
 *
 * The excerpt arrives *centred on the match* now, so its first two lines really
 * do contain the search term — except on a `path` match, where by definition
 * they cannot, and the row says so instead of leaving the reader to wonder why
 * a result they did not ask for is on screen.
 */

import { memo } from "react";

import styles from "./ResultRow.module.css";
import { cx } from "../lib/cx";
import { age, dirOf, squeezeDir } from "../lib/format";
import { segments } from "../lib/highlight";
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
 *   26px  row padding (9+9) plus the excerpt plate's own padding (4+4)
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

function Marked({ text, terms }: { text: string; terms: readonly string[] }) {
  return (
    <>
      {segments(text, terms).map((s, i) =>
        s.hit ? (
          <mark key={i} className={styles.mark}>
            {s.text}
          </mark>
        ) : (
          <span key={i}>{s.text}</span>
        ),
      )}
    </>
  );
}

export interface ResultRowProps {
  hit: SearchHit;
  selected: boolean;
  /** 1-based; shown as a `⌘n` cap for the first nine only. */
  ordinal: number;
  /** The query's distinct terms, for marking. Never re-derived per row. */
  terms: readonly string[];
  onSelect: (hit: SearchHit) => void;
  onOpen: (hit: SearchHit) => void;
}

export const ResultRow = memo(function ResultRow({
  hit,
  selected,
  ordinal,
  terms,
  onSelect,
  onOpen,
}: ResultRowProps) {
  const dir = dirOf(hit.relativePath);
  const base = hit.relativePath.slice(dir.length);
  const shownDir = squeezeDir(dir);
  const suffix = hit.line === null ? "" : `:${hit.line}`;
  const lines = excerptLines(hit.excerpt);
  const pathMatch = hit.reason === "path";

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
      {/* 1 — the citation, first. Marked, because on a path match this is
             where the term is and the excerpt below is not. */}
      <div className={styles.head}>
        <span className={styles.location} title={hit.location}>
          {dir !== "" && (
            <span className={styles.dir}>
              <Marked text={shownDir} terms={terms} />
            </span>
          )}
          <span className={styles.base}>
            <Marked text={base} terms={terms} />
          </span>
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

      {/* 3 — the matched content.
             On a path match the excerpt is the start of the file and cannot
             contain the query, so the plate says what it is showing rather
             than presenting the file's first lines as if they were the hit.
             That row — an excerpt with no search term in it and no explanation
             — is what "its confusing" was about. */}
      <div
        className={cx("mono", styles.excerpt, pathMatch && styles.excerptAside)}
      >
        {pathMatch && (
          <div className={styles.pathNote}>
            matched in the file name · showing the start of the file
          </div>
        )}
        {lines.slice(0, pathMatch ? 1 : 2).map((l, i) => (
          <div key={i} className={styles.excerptLine}>
            <Marked text={l} terms={terms} />
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
