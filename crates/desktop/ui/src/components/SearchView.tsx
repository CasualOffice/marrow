/**
 * The search view: field, results, detail (GUI §3.2) — Enclave's list beside a
 * peek panel, which is the same shape as this one.
 *
 * **The pane is reserved for a selection, not for the whole session.** Three
 * states, and only one of them is a split:
 *
 *   nothing to detail  → one full-width surface, one quiet line
 *   searched, no hits  → one full-width surface, the ZeroResults diagnosis
 *   a result selected  → results column beside the detail pane
 *
 * The first of those is the bug this shape fixes. The detail pane held a fixed
 * 62% of the window from launch, and until a result was selected the only thing
 * in it was the sentence "N files indexed across M workspaces" — centred in
 * ~920px of otherwise empty sheet, beside a results column that was *also*
 * showing a centred sentence. Two empty states, drawn as two panes, saying two
 * halves of one thing.
 *
 * What did not happen here, and why, because "fill it with something" is the
 * obvious move and every version of it is wrong:
 *
 *  - **Not a dashboard, and not tips.** GUI §4 is explicit — "Search is the
 *    launch view. Not a dashboard, not an empty state with tips." DetailPane's
 *    own idle branch carries the same note.
 *  - **Not example queries.** The desktop `search` command takes a query string
 *    and a limit (`api.ts`, `state.rs::search_semantic`) — prefix-matched, one
 *    lexical branch. `--type`, `--since` and `--literal` are `marrow search`
 *    flags in `crates/cli/src/main.rs` and reach no part of this field.
 *    Advertising them here would be teaching a syntax the app silently ignores,
 *    which is the "control that lies" class this codebase keeps finding.
 *  - **Not recent files.** `list_files` returns newest-first and would have
 *    fitted, but `ResultList`/`onSelect` speak `SearchHit`, and a `FileRow`
 *    has no provenance, no reason and no citability. Building one would mean
 *    inventing `provenance: "exact"` for a row nothing matched — hard rule 1,
 *    from the empty state of the view whose entire purpose is provenance.
 *  - **Not the workspace counts.** The sidebar is already showing them, with
 *    their problem lines, three inches to the left.
 *
 * So the honest answer is the one the task allows: there is nothing to detail
 * yet, so nothing claims the width. The results column takes the sheet, and the
 * field is capped so it does not move when the split opens.
 */

import type { ReactNode, RefObject } from "react";

import styles from "./SearchView.module.css";
import { count, ms } from "../lib/format";
import { SearchField } from "./SearchField";
import { ResultList } from "./ResultList";
import { DetailPane } from "./DetailPane";
import { ZeroResults } from "./ZeroResults";
import { ErrorNotice } from "./ErrorNotice";
import { useUi, type Anchor } from "../store";
import type { SearchHit, SearchResponse, UiError } from "../api";

export interface SearchViewProps {
  query: string;
  onQueryChange: (q: string) => void;
  response: SearchResponse | undefined;
  error: UiError | null;
  /** True only once a fetch has outlived its budget. Never flashes. */
  slow: boolean;
  anchor: Anchor | null;
  selectedHit: SearchHit | null;
  scrollNonce: number;
  onSelect: (hit: SearchHit) => void;
  onOpen: (hit: SearchHit) => void;
  searchRef: RefObject<HTMLInputElement>;
  resultsRef: RefObject<HTMLDivElement>;
  detailRef: RefObject<HTMLDivElement>;
  idle: ReactNode;
}

export function SearchView(props: SearchViewProps) {
  const {
    query,
    onQueryChange,
    response,
    error,
    slow,
    anchor,
    selectedHit,
    scrollNonce,
    onSelect,
    onOpen,
    searchRef,
    resultsRef,
    detailRef,
    idle,
  } = props;

  const focusPane = useUi((s) => s.focusPane);
  const hits = response?.hits ?? [];

  // A query that has been asked and answered with nothing. A query still in
  // flight keeps the previous ranking on screen instead (queries.ts).
  const answered = response !== undefined && response.query === query.trim();
  const zero = query.trim() !== "" && answered && hits.length === 0;

  /*
   * The split opens for a ranking, not for a session.
   *
   * Keyed on `hits`, deliberately not on `anchor === null` even though the
   * anchor is what the pane renders. App anchors the first hit from a *passive*
   * effect, which runs after paint: keying the layout on the anchor would paint
   * one full-width frame with a list already in it, then reflow to the split on
   * the next tick. Keying on the hits means the two arrive in the same commit.
   * The pane's own null-anchor branch covers that single frame and draws
   * nothing, which is why no `idle` is passed to it from here.
   */
  const split = !zero && hits.length > 0;

  // One full-width surface for both empty states. They are different states —
  // "not yet asked" and "asked, nothing there" — but they are the same shape,
  // and giving them different widths made the field jump between them.
  const wide = !split;

  const footer = (
    <div className={styles.footer} role="status">
      <span className={styles.count}>
        {response === undefined
          ? "—"
          : // `matched`, not `total`: the latter saturates at the page size, so
            // a footer built on it reports "200 results" for a corpus holding
            // 842 and the user has no way to detect the difference.
            `${count(response.matched)} ${response.matched === 1 ? "result" : "results"}`}
      </span>
      <span className={styles.sep} aria-hidden="true">
        ·
      </span>
      <span className="mono">
        {response === undefined ? "—" : ms(response.elapsedMs)}
      </span>
      {slow && (
        <>
          <span className={styles.sep} aria-hidden="true">
            ·
          </span>
          <span className={styles.slow}>searching</span>
        </>
      )}
      <span className={styles.grow} />
      <span className="mono">
        {response === undefined || response.branches.length === 0
          ? "—"
          : response.branches.join(" + ")}
      </span>
    </div>
  );

  // ONE SearchField, rendered outside every conditional.
  //
  // It used to be duplicated across the zero-results branch and the results
  // branch. Typing the first character produced results, React swapped
  // branches, and the input was unmounted and remounted — so focus and the
  // caret were lost on every single keystroke. A conditional around an input
  // is a remount, not a re-render.
  const field = (
    <div className={styles.fieldRow}>
      <SearchField
        ref={searchRef}
        value={query}
        onChange={onQueryChange}
        placeholder="Search everything indexed"
        label="Search"
        cap="⌘F"
      />
    </div>
  );

  // The tree shape is identical in every state, so the input keeps its
  // position and is never remounted. Only the className and the block below
  // the field change — and note that even the error row and the list/zero
  // swap are *after* the field, never around it. React reconciles by position:
  // rendering the same element in two different branches still remounts it.
  return (
    <>
      <div
        ref={resultsRef}
        tabIndex={-1}
        className={wide ? styles.wideArea : styles.results}
        onFocus={() => focusPane("results")}
      >
        {field}

        {error !== null && !zero && (
          <div className={styles.errorRow}>
            <ErrorNotice error={error} action={null} compact />
          </div>
        )}

        {zero ? (
          <ZeroResults
            query={query.trim()}
            elapsedMs={response?.elapsedMs ?? 0}
            error={error}
            onTry={onQueryChange}
          />
        ) : split ? (
          <ResultList
            hits={hits}
            selectedKey={anchor?.key ?? null}
            scrollNonce={scrollNonce}
            onSelect={onSelect}
            onOpen={onOpen}
          />
        ) : (
          /*
           * The two sentences that used to sit in two panes, as one block on
           * one surface: what the field does, then how much it reaches over.
           * That is the whole of it.
           *
           * The second line is the one worth keeping. The first repeats the
           * field's own placeholder and is here only so the block still says
           * something while the counts are in flight; the scope line is the
           * only place in this view that answers "everything indexed" with a
           * number.
           *
           * This also unmounts `ResultList` while there is nothing to list.
           * That is safe for the one thing it could have broken — the field is
           * an earlier sibling, and React reconciles by position, so swapping
           * a later child's type does not remount an earlier one. The caret
           * survives; it is the virtualiser that is rebuilt, on a query that
           * wants a fresh scroll position anyway.
           *
           * `idle` is null until `index_health` and `list_workspaces` have
           * both answered. It renders as nothing rather than as a zero: a
           * count that has not arrived is not a count of none (UX principle
           * 5), and "0 files indexed" on launch is a false alarm about the
           * index rather than a true statement about a query in flight.
           */
          <div className={styles.blank}>
            <p className={styles.blankPrompt}>Type to search everything indexed.</p>
            {idle && <p className={styles.blankScope}>{idle}</p>}
          </div>
        )}

        {footer}
      </div>

      {split && (
        <DetailPane ref={detailRef} anchor={anchor} hit={selectedHit} />
      )}
    </>
  );
}
