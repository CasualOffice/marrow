/**
 * Files — "browse by workspace/folder; the index as a filesystem you can trust"
 * (GUI §4), laid out like design/FileDetail.dc.html: a 280px file column beside
 * the intelligence panel.
 *
 * There is no `list_files` command, so this browses the index through the one
 * command that returns files: `search`, deduplicated by `fileId`. That is why
 * the column is headed by a filter field rather than a folder tree — the shape
 * of the command surface is visible in the shape of the UI, which is the honest
 * outcome. The missing command is named in the accompanying report.
 */

import { useMemo, useRef, useState, type RefObject } from "react";

import styles from "./FilesView.module.css";
import { cx } from "../lib/cx";
import { baseOf, count, dirOf } from "../lib/format";
import { Icon } from "./Icon";
import { SearchField } from "./SearchField";
import { DetailPane } from "./DetailPane";
import { ErrorNotice } from "./ErrorNotice";
import { useDebounced, useSearch } from "../queries";
import { useUi, type Anchor } from "../store";
import type { SearchHit } from "../api";

/** One row per file, not per chunk. The first hit wins the excerpt. */
function uniqueFiles(hits: readonly SearchHit[]): SearchHit[] {
  const seen = new Set<string>();
  const out: SearchHit[] = [];
  for (const h of hits) {
    if (seen.has(h.fileId)) continue;
    seen.add(h.fileId);
    out.push(h);
  }
  return out;
}

export function FilesView({
  detailRef,
}: {
  detailRef: RefObject<HTMLDivElement>;
}) {
  const [filter, setFilter] = useState("");
  const [anchor, setAnchor] = useState<Anchor | null>(null);
  const fieldRef = useRef<HTMLInputElement>(null);

  const debounced = useDebounced(filter);
  const q = useSearch(debounced, 200);
  const files = useMemo(() => uniqueFiles(q.data?.hits ?? []), [q.data]);
  const selectedHit = files.find((f) => f.path === anchor?.path) ?? null;
  const notify = useUi((s) => s.notify);

  return (
    <>
      <div className={styles.column}>
        <div className={styles.head}>
          <h2 className={styles.heading}>Files</h2>
          <span className={styles.headCount}>
            {q.data ? count(files.length) : "—"}
          </span>
        </div>
        <div className={styles.filterRow}>
          <SearchField
            ref={fieldRef}
            value={filter}
            onChange={setFilter}
            placeholder="Filter by name or content"
            label="Filter files"
          />
        </div>

        {q.isError ? (
          <div className={styles.pad}>
            <ErrorNotice error={q.error} action={null} compact />
          </div>
        ) : (
          <ul className={styles.list}>
            {files.map((f) => {
              const selected = f.path === anchor?.path;
              return (
                <li key={f.fileId}>
                  <button
                    type="button"
                    className={cx(styles.row, selected && styles.selected)}
                    onClick={() =>
                      setAnchor({
                        key: f.fileId,
                        path: f.path,
                        relativePath: f.relativePath,
                        // The file, not a match inside it: the preview starts
                        // at line 1 rather than at somebody else's hit.
                        line: null,
                        location: f.relativePath,
                      })
                    }
                  >
                    <Icon name="file" size={13} className={styles.rowIcon} />
                    <span className={styles.rowName}>
                      {baseOf(f.relativePath)}
                    </span>
                    <span className={cx("mono", styles.rowDir)}>
                      {dirOf(f.relativePath)}
                    </span>
                  </button>
                </li>
              );
            })}
            {files.length === 0 && (
              <li className={styles.none}>
                {filter.trim() === ""
                  ? "Type to filter the index."
                  : q.data === undefined
                    ? "—"
                    : "Nothing in the index matches that."}
              </li>
            )}
          </ul>
        )}

        <div className={styles.foot}>
          <button
            type="button"
            className={styles.footNote}
            onClick={() =>
              notify(
                'Browsing folders needs a desktop command that does not exist yet ("list_files"). This column filters the index instead.',
              )
            }
          >
            Filtered from the index
          </button>
        </div>
      </div>

      <DetailPane
        ref={detailRef}
        anchor={anchor}
        hit={selectedHit}
        idle="Pick a file to see everything the index knows about it."
      />
    </>
  );
}
