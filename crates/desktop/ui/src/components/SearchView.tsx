/**
 * The search view: field, results, detail (GUI §3.2) — Enclave's list beside a
 * peek panel, which is the same shape as this one.
 *
 * When the ranking is empty the results pane gives way to the diagnosis, which
 * spans the width the two panes had.
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
        className={zero ? styles.wideArea : styles.results}
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
        ) : (
          <ResultList
            hits={hits}
            selectedKey={anchor?.key ?? null}
            scrollNonce={scrollNonce}
            onSelect={onSelect}
            onOpen={onOpen}
          />
        )}

        {footer}
      </div>

      {!zero && (
        <DetailPane
          ref={detailRef}
          anchor={anchor}
          hit={selectedHit}
          idle={idle}
        />
      )}
    </>
  );
}
