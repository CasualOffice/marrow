/**
 * Files — "browse by workspace/folder; the index as a filesystem you can trust"
 * (GUI §4).
 *
 * **Built on `list_files`, not on `search`.** It used to be built on `search`,
 * which needs a query, so with no query it rendered "Type to filter the index."
 * over an index holding 35,000 files — a browser that browses nothing. Browsing
 * is not searching, and it now has its own command:
 *
 *   • rows appear immediately, newest first, with no query at all
 *   • the box filters by path prefix/substring, server-side
 *   • the sidebar scopes it to one workspace
 *   • `metadataOnly` rows are marked, because a file you can find by name but
 *     not by what is inside it is a different thing from an indexed one, and a
 *     list that draws them identically is lying by omission
 *
 * Layout is Enclave's list-beside-peek: pick a row on the left, read it on the
 * right, without leaving the list.
 */

import { useMemo, useRef, useState, type RefObject } from "react";

import styles from "./FilesView.module.css";
import { cx } from "../lib/cx";
import { age, baseOf, bytes, count, dirOf, squeezeDir } from "../lib/format";
import { Icon } from "./Icon";
import { SearchField } from "./SearchField";
import { DetailPane } from "./DetailPane";
import { ErrorNotice } from "./ErrorNotice";
import { useDebounced, useFiles, FILES_LIMIT } from "../queries";
import { useUi, type Anchor } from "../store";

export function FilesView({
  detailRef,
}: {
  detailRef: RefObject<HTMLDivElement>;
}) {
  const [filter, setFilter] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  const fieldRef = useRef<HTMLInputElement>(null);

  const workspace = useUi((s) => s.workspaceFilter);
  const setWorkspace = useUi((s) => s.setWorkspaceFilter);

  const debounced = useDebounced(filter);
  const q = useFiles(workspace, debounced);
  const rows = useMemo(() => q.data ?? [], [q.data]);

  const row = rows.find((f) => f.path === selected) ?? null;
  const anchor: Anchor | null =
    row === null
      ? null
      : {
          key: row.path,
          path: row.path,
          relativePath: row.relativePath,
          // The file, not a match inside it: the preview starts at line 1
          // rather than at somebody else's hit.
          line: null,
          location: row.relativePath,
        };

  const metadataOnly = rows.filter((f) => f.metadataOnly).length;
  const capped = rows.length >= FILES_LIMIT;

  return (
    <>
      <div className={styles.column}>
        <div className={styles.head}>
          <h2 className={styles.heading}>
            {workspace ?? "All workspaces"}
          </h2>
          {workspace !== null && (
            <button
              type="button"
              className={styles.clear}
              onClick={() => setWorkspace(null)}
            >
              <Icon name="close" size={10} />
              <span className="srOnly">Show all workspaces</span>
            </button>
          )}
          <span className={cx("mono", styles.headCount)}>
            {q.data === undefined ? "—" : count(rows.length)}
          </span>
        </div>

        {/* Outside every conditional below, so the input is never remounted by
            a branch swap while it is being typed into. */}
        <div className={styles.filterRow}>
          <SearchField
            ref={fieldRef}
            value={filter}
            onChange={setFilter}
            placeholder="Filter by path"
            label="Filter files by path"
          />
        </div>

        {q.isError && (
          <div className={styles.pad}>
            <ErrorNotice error={q.error} action={null} compact />
          </div>
        )}

        <ul className={styles.list}>
          {rows.map((f) => {
            const dir = dirOf(f.relativePath);
            return (
              <li key={f.path}>
                <button
                  type="button"
                  aria-current={f.path === selected ? "true" : undefined}
                  className={cx(
                    styles.row,
                    f.path === selected && styles.selected,
                  )}
                  onClick={() => setSelected(f.path)}
                >
                  <Icon
                    name={f.metadataOnly ? "fileDim" : "file"}
                    size={14}
                    className={cx(
                      styles.rowIcon,
                      f.metadataOnly && styles.rowIconDim,
                    )}
                  />
                  <span className={styles.rowMain}>
                    <span className={styles.rowName}>
                      {dir !== "" && (
                        <span className={styles.rowDir}>{squeezeDir(dir)}</span>
                      )}
                      <span className={styles.rowBase}>
                        {baseOf(f.relativePath)}
                      </span>
                    </span>
                    {/*
                      The whole reason this row is different. `metadataOnly`
                      means the file is in the index by name and date only —
                      searching its contents will never find it — so it says
                      so in words, and the tint is only reinforcement.
                    */}
                    {f.metadataOnly ? (
                      <span className={styles.rowNote}>
                        name and date only · contents not searchable
                      </span>
                    ) : (
                      <span className={cx("mono", styles.rowMeta)}>
                        {count(f.chunks)} chunks · {bytes(f.sizeBytes)}
                      </span>
                    )}
                  </span>
                  <span className={cx("mono", styles.rowAge)}>
                    {f.modifiedMs === null ? "—" : age(f.modifiedMs)}
                  </span>
                </button>
              </li>
            );
          })}

          {!q.isError && rows.length === 0 && (
            <li className={styles.none}>
              {q.data === undefined
                ? "—"
                : filter.trim() === ""
                  ? workspace === null
                    ? "The index is empty. Nothing has been indexed yet."
                    : `Nothing is indexed in ${workspace}.`
                  : "No indexed path contains that."}
            </li>
          )}
        </ul>

        {/* Honest about the window: the list is the newest N, not all of them,
            and it says which. */}
        <div className={styles.foot}>
          <span>
            {q.data === undefined
              ? "—"
              : capped
                ? `newest ${count(rows.length)}`
                : `${count(rows.length)} ${rows.length === 1 ? "file" : "files"}`}
          </span>
          {metadataOnly > 0 && (
            <>
              <span aria-hidden="true">·</span>
              <span className={styles.footWarn}>
                {count(metadataOnly)} not searchable
              </span>
            </>
          )}
          <span className={styles.grow} />
          <span>newest first</span>
        </div>
      </div>

      <DetailPane
        ref={detailRef}
        anchor={anchor}
        hit={null}
        idle="Pick a file to see everything the index knows about it."
      />
    </>
  );
}
