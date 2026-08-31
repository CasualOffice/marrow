/**
 * Detail — "Preview with the match highlighted in context, plus the
 * file-intelligence panel below the fold" (GUI §3.2).
 *
 * The preview is bounded by the core, not by this pane: `read_region` returns
 * the matched region of a 50 MB file, never the whole file (GUI §7).
 */

import { forwardRef, useEffect, useRef, type ReactNode } from "react";

import styles from "./DetailPane.module.css";
import { cx } from "../lib/cx";
import { ageLong, baseOf, bytes, count, DASH, extOf } from "../lib/format";
import { Icon } from "./Icon";
import { Kbd } from "./Kbd";
import { ProvenanceBadge, ReasonBadge, SelfBadge } from "./Badges";
import { ErrorNotice } from "./ErrorNotice";
import { useFileDetail, useRegion } from "../queries";
import { useUi, type Anchor } from "../store";
import {
  copyCitation,
  openInSystem,
  revealInFileManager,
} from "../actions";
import type { SearchHit } from "../api";

export interface DetailPaneProps {
  anchor: Anchor | null;
  /** The hit the anchor came from, when it is still in the current ranking. */
  hit: SearchHit | null;
  /**
   * Rendered when nothing is selected. Kept quiet: this is not an empty state
   * with tips (GUI §4).
   *
   * Files passes one, because its list is there from launch and "pick a file"
   * is a real instruction about a real list. Search passes none and is not
   * mounted at all until a result is selected — a pane whose only content was
   * a sentence held 62% of that window from launch, and the sentence has moved
   * to the surface it describes. So on the Search path this branch renders for
   * the single frame between a ranking arriving and App anchoring its first
   * hit, and it must draw nothing at all in that frame.
   */
  idle?: ReactNode;
}

export const DetailPane = forwardRef<HTMLDivElement, DetailPaneProps>(
  function DetailPane({ anchor, hit, idle }, ref) {
    const focusPane = useUi((s) => s.focusPane);
    const path = anchor?.path ?? null;
    const detail = useFileDetail(path);
    const region = useRegion(path, anchor?.line ?? null);

    const matchedRef = useRef<HTMLDivElement>(null);
    useEffect(() => {
      matchedRef.current?.scrollIntoView({ block: "center" });
    }, [path, anchor?.line, region.data]);

    return (
      <section
        ref={ref}
        tabIndex={-1}
        className={styles.pane}
        aria-label="File detail"
        onFocus={() => focusPane("detail")}
      >
        {anchor === null ? (
          <div className={styles.idle}>{idle}</div>
        ) : (
          <>
            <header className={styles.header}>
              <div className={styles.headRow}>
                <h1 className={styles.name}>{baseOf(anchor.relativePath)}</h1>
                {hit && <ReasonBadge reason={hit.reason} />}
                {hit && <ProvenanceBadge provenance={hit.provenance} />}
                {detail.data && <SelfBadge citable={detail.data.citable} />}
                <span className={styles.grow} />
                <button
                  type="button"
                  className={styles.capButton}
                  title="Open in the system's default application"
                  onClick={() =>
                    void openInSystem(
                      anchor.path,
                      baseOf(anchor.relativePath),
                    )
                  }
                >
                  <Kbd label="Command Return">⌘↵</Kbd>
                  <span className={styles.capLabel}>open</span>
                </button>
                <button
                  type="button"
                  className={styles.capButton}
                  title="Reveal in the file manager"
                  onClick={() =>
                    void revealInFileManager(
                      anchor.path,
                      baseOf(anchor.relativePath),
                    )
                  }
                >
                  <Kbd label="Shift Return">⇧↵</Kbd>
                  <span className={styles.capLabel}>reveal</span>
                </button>
                <button
                  type="button"
                  className={styles.capButton}
                  onClick={() => void copyCitation(anchor)}
                >
                  <Kbd label="Command C">⌘C</Kbd>
                  <span className={styles.capLabel}>cite</span>
                </button>
              </div>
              <div className={cx("mono", styles.path, "selectable")}>
                {anchor.location}
              </div>
            </header>

            <div className={styles.preview}>
              {region.isError ? (
                <div className={styles.previewError}>
                  <ErrorNotice
                    error={region.error}
                    /* No action for a cloud placeholder. Hard rule 3 makes
                       this a refusal rather than a missing command, and
                       "Download it" calling `unavailable("hydrate")` read as
                       "not built yet". The error's own message already names
                       the cause and what to do — open it in its own app —
                       which is more than the button ever did. */
                    action={null}
                  />
                </div>
              ) : (
                <div className={styles.lines} role="group" aria-label="Preview">
                  {(region.data?.lines ?? ([] as readonly string[])).map((src: string, i: number) => {
                    const n = (region.data?.firstLine ?? 1) + i;
                    const matched = anchor.line !== null && n === anchor.line;
                    return (
                      <div
                        key={n}
                        ref={matched ? matchedRef : undefined}
                        className={cx(styles.line, matched && styles.matched)}
                      >
                        <span className={cx("mono", styles.lineNo)}>{n}</span>
                        <span className={cx("mono", styles.src, "selectable")}>
                          {src === "" ? " " : src}
                        </span>
                        {matched && <span className="srOnly">matched line</span>}
                      </div>
                    );
                  })}
                  {region.data?.truncated === true && (
                    <div className={styles.truncated}>
                      <span className={styles.truncatedRule} />
                      <span>
                        cut off here by the preview cap, not by the end of the
                        file
                      </span>
                      <span className={styles.truncatedRule} />
                    </div>
                  )}
                </div>
              )}
            </div>

            <footer className={styles.intel}>
              <div className={styles.sectionHead}>
                <h2 className={styles.sectionTitle}>This file</h2>
                <div className={styles.rule} />
                <Icon name="chevron" size={12} className={styles.chevron} />
              </div>

              {detail.isError ? (
                <ErrorNotice error={detail.error} action={null} />
              ) : (
                <>
                  <dl className={styles.grid}>
                    <Fact k="modified" v={ageLong(detail.data?.modifiedMs ?? null)} />
                    <Fact k="size" v={bytes(detail.data?.sizeBytes ?? null)} />
                    <Fact k="type" v={detail.data?.mime ?? extOf(anchor.relativePath)} />
                    <Fact
                      k="provenance"
                      v={hit ? hit.provenance : DASH}
                      tone={hit && hit.provenance !== "exact" ? "warn" : undefined}
                    />
                    <Fact k="workspace" v={detail.data?.workspace ?? DASH} />
                    <Fact k="chunks" v={count(detail.data?.chunks)} />
                    <Fact k="versions" v={count(detail.data?.versions)} />
                    <Fact
                      k="tier"
                      v={detail.data?.tierState ?? DASH}
                      tone={
                        detail.data && detail.data.tierState !== "resident"
                          ? "warn"
                          : undefined
                      }
                    />
                  </dl>

                  <dl className={styles.grid}>
                    <Fact
                      k="file id"
                      v={detail.data?.fileId ?? DASH}
                      mono
                      span={2}
                    />
                    <Fact
                      k="content"
                      v={detail.data?.contentHash ?? DASH}
                      mono
                      span={2}
                    />
                  </dl>

                  {/* M1 extracts neither. `—` reads as "we looked, nothing
                      there"; blank reads as "fine" (UX principle 5). */}
                  <dl className={styles.grid}>
                    <Fact
                      k="embedded metadata"
                      v={detail.data?.embeddedMetadata == null ? DASH : "present"}
                      span={2}
                    />
                    <Fact
                      k="structure"
                      v={detail.data?.structure == null ? DASH : "present"}
                      span={2}
                    />
                  </dl>

                  {detail.data && detail.data.previousPaths.length > 0 && (
                    <div className={styles.previous}>
                      <span className={styles.factKey}>earlier paths</span>
                      <ul className={styles.previousList}>
                        {detail.data.previousPaths.map((p) => (
                          <li key={p} className={cx("mono", styles.previousItem)}>
                            {p}
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                </>
              )}
            </footer>
          </>
        )}
      </section>
    );
  },
);

function Fact({
  k,
  v,
  mono,
  tone,
  span,
}: {
  k: string;
  v: string;
  mono?: boolean | undefined;
  tone?: "warn" | undefined;
  span?: number | undefined;
}) {
  return (
    <div
      className={styles.fact}
      style={span ? { gridColumn: `span ${span}` } : undefined}
    >
      <dt className={styles.factKey}>{k}</dt>
      <dd
        className={cx(
          styles.factValue,
          mono && "mono",
          tone === "warn" && styles.warn,
          v === DASH && styles.absent,
        )}
      >
        {v}
      </dd>
    </div>
  );
}
