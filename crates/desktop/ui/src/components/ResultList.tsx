/**
 * The result list. Virtualized from the first row, not after some threshold
 * (GUI §7), and keyed by result identity so a re-rank moves rows without
 * re-mounting them.
 *
 * Two rules from GUI §5.2 live here:
 *
 *   • Rows are absolutely positioned and transformed, and their React key is
 *     `fileId#line`. When the ranking changes, the same DOM node keeps its
 *     identity and only its `translateY` changes, so the browser animates the
 *     move instead of thrashing layout.
 *   • The list only scrolls when the *user* moved the cursor. A re-rank that
 *     relocates the selected row must not also yank the viewport — the cursor
 *     is still on the same result, which is the promise being kept.
 */

import { useLayoutEffect, useRef } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";

import styles from "./ResultList.module.css";
import { ResultRow, ROW_HEIGHT } from "./ResultRow";
import { hitKey } from "../store";
import type { SearchHit } from "../api";

export interface ResultListProps {
  hits: readonly SearchHit[];
  selectedKey: string | null;
  /** Bumped by the caller only when a keypress moved the cursor. */
  scrollNonce: number;
  onSelect: (hit: SearchHit) => void;
  onOpen: (hit: SearchHit) => void;
}

export function ResultList({
  hits,
  selectedKey,
  scrollNonce,
  onSelect,
  onOpen,
}: ResultListProps) {
  const parentRef = useRef<HTMLDivElement>(null);

  const virtualizer = useVirtualizer({
    count: hits.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 8,
    getItemKey: (index) => {
      const h = hits[index];
      return h ? hitKey(h) : index;
    },
  });

  const selectedIndex = selectedKey
    ? hits.findIndex((h) => hitKey(h) === selectedKey)
    : -1;

  // Only ever in response to a keypress. See the note at the top of the file.
  const lastNonce = useRef(scrollNonce);
  useLayoutEffect(() => {
    if (lastNonce.current === scrollNonce) return;
    lastNonce.current = scrollNonce;
    if (selectedIndex >= 0) {
      virtualizer.scrollToIndex(selectedIndex, { align: "auto" });
    }
  }, [scrollNonce, selectedIndex, virtualizer]);

  const items = virtualizer.getVirtualItems();

  return (
    <div
      ref={parentRef}
      className={styles.scroller}
      style={{ ["--result-row-h" as string]: `${ROW_HEIGHT}px` }}
    >
      <div
        role="listbox"
        aria-label="Search results"
        aria-activedescendant={
          selectedIndex >= 0 && hits[selectedIndex]
            ? `result-${hits[selectedIndex].fileId}-${hits[selectedIndex].line ?? "x"}`
            : undefined
        }
        className={styles.canvas}
        style={{ height: `${virtualizer.getTotalSize()}px` }}
      >
        {items.map((item) => {
          const hit = hits[item.index];
          if (!hit) return null;
          const key = hitKey(hit);
          return (
            <div
              key={key}
              className={styles.slot}
              style={{ transform: `translateY(${item.start}px)` }}
            >
              <ResultRow
                hit={hit}
                ordinal={item.index + 1}
                selected={key === selectedKey}
                onSelect={onSelect}
                onOpen={onOpen}
              />
            </div>
          );
        })}
      </div>
    </div>
  );
}
