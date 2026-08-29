/**
 * The search view: field, results, detail (GUI §3.2).
 *
 * Layout is the mockup's: sidebar 180 · results 360 · detail fills. When the
 * ranking is empty the results pane gives way to the diagnosis, which spans the
 * width the two panes had — design/ZeroResults.dc.html.
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
      <span>
        {response === undefined
          ? "—"
          : // `matched` not `total`: the latter saturates at the page size, so a
            // footer built on it reports "20 results" for a corpus holding 900.
            `${count(response.matched)} ${response.matched === 1 ? "result" : "results"}`}
      </span>
      <span aria-hidden="true">·</span>
      <span className="mono">{response === undefined ? "—" : ms(response.elapsedMs)}</span>
      {slow && (
        <>
          <span aria-hidden="true">·</span>
          <span className={styles.slow}>searching</span>
        </>
      )}
      <span className={styles.grow} />
      <span>
        {response === undefined || response.branches.length === 0
          ? "—"
          : response.branches.join(" + ")}
      </span>
    </div>
  );

  if (zero) {
    return (
      <div className={styles.wideArea}>
        <div className={styles.fieldRow}>
          <div className={styles.fieldWide}>
            <SearchField
              ref={searchRef}
              value={query}
              onChange={onQueryChange}
              placeholder="Search everything indexed"
              label="Search"
              cap="⌘F"
            />
          </div>
        </div>
        <ZeroResults
          query={query.trim()}
          elapsedMs={response?.elapsedMs ?? 0}
          error={error}
          onTry={onQueryChange}
        />
        {footer}
      </div>
    );
  }

  return (
    <>
      <div
        ref={resultsRef}
        tabIndex={-1}
        className={styles.results}
        onFocus={() => focusPane("results")}
      >
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

        {error !== null && (
          <div className={styles.errorRow}>
            <ErrorNotice error={error} action={null} compact />
          </div>
        )}

        <ResultList
          hits={hits}
          selectedKey={anchor?.key ?? null}
          scrollNonce={scrollNonce}
          onSelect={onSelect}
          onOpen={onOpen}
        />

        {footer}
      </div>

      <DetailPane
        ref={detailRef}
        anchor={anchor}
        hit={selectedHit}
        idle={idle}
      />
    </>
  );
}
