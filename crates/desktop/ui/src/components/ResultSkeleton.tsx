/**
 * The shape of a ranking that has not arrived yet (SKEL-001).
 *
 * A spinner says "wait". This says "a list of results is coming, and it will
 * sit exactly here" — the rows are the same height as real ones, so when the
 * ranking lands nothing moves (SKEL-004). That the layout does not jump is the
 * whole reason to draw the shape rather than a symbol.
 *
 * **It is not silence to a screen reader** (SKEL-008) — which took two goes.
 * The bars are `aria-hidden`, because "decorative rectangle" thirty-two times
 * is worse than nothing; but a live region announces its *content*, and
 * `aria-label` supplies a name rather than content, so hiding every child left
 * exactly the silence the rule forbids. There is now real text in the region.
 *
 * And `aria-busy` is gone from it. `aria-busy="true"` on a live region means
 * "do not announce updates yet", released by flipping it to `false` — which
 * never happens here, because the region is unmounted rather than settled. A
 * region that is busy for its whole life is a region that never speaks.
 */
import styles from "./ResultSkeleton.module.css";
import { ROW_HEIGHT } from "./ResultRow";

/**
 * Widths that look like paths and excerpts rather than a bar chart.
 *
 * Four bars per row, because a real row is four things: a path with its age
 * beside it, two lines of excerpt, and the breadcrumb under them. Two bars in
 * a row of this height left a gap down the middle that read as loose spacing
 * rather than as a document.
 */
const ROWS = [
  { head: 62, a: 88, b: 71, crumb: 34 },
  { head: 45, a: 74, b: 52, crumb: 28 },
  { head: 71, a: 91, b: 66, crumb: 41 },
  { head: 38, a: 66, b: 79, crumb: 24 },
  { head: 58, a: 83, b: 61, crumb: 37 },
  { head: 49, a: 78, b: 84, crumb: 30 },
  { head: 66, a: 70, b: 58, crumb: 45 },
  { head: 41, a: 86, b: 73, crumb: 26 },
];

export function ResultSkeleton({ label }: { label: string }) {
  return (
    <div
      className={styles.list}
      /* The real row height, from the same constant `ResultList` measures its
         virtualiser with. Read rather than restated: a skeleton whose rows are
         a different height than the rows replacing them jumps on arrival,
         which is the one thing it exists to prevent. */
      style={{ ["--result-row-h" as string]: `${ROW_HEIGHT}px` }}
      role="status"
      aria-live="polite"
    >
      <span className="srOnly">{label}</span>
      {ROWS.map((r, i) => (
        <div key={i} className={styles.row} aria-hidden="true">
          <div className={styles.head}>
            <span className={styles.bar} style={{ width: `${r.head}%` }} />
            <span className={styles.meta} />
          </div>
          <div className={styles.excerpt}>
            <span className={styles.bar} style={{ width: `${r.a}%` }} />
            <span className={styles.bar} style={{ width: `${r.b}%` }} />
          </div>
          <span className={styles.crumb} style={{ width: `${r.crumb}%` }} />
        </div>
      ))}
    </div>
  );
}
