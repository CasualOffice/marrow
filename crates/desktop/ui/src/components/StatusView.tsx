/**
 * Status — index health, per workspace and in total.
 *
 * `list_workspaces` returns `chunks`, `contentBytes`, `cloudOnly` and the
 * unindexed breakdown *per workspace* now, so the stats that used to render as
 * `—` are real numbers. The em dash is reserved for what the backend genuinely
 * cannot answer — watcher state, when the workspace was last indexed, when it
 * was last reconciled — and each of those says what would have to exist for it
 * to be a number, rather than sitting there as a decorative dash.
 *
 * "An empty cell reads as 'fine'; `—` reads as 'we looked, nothing there'"
 * (UX principle 5). The corollary this view has to honour: a dash where a
 * number is available reads as a broken page.
 *
 * **This page used to alarm about normal behaviour.** `unindexed` — files with
 * no chunks — was rendered as one number under a warning triangle reading
 * "their contents were never read", and on the author's index that was 42,581
 * photos, fonts and binaries with no parser. Zero parses had actually failed. A
 * file with no parser stays discoverable via metadata (T5) and is not a
 * failure, so a warning that can never be cleared trains the eye to skip the
 * one that can.
 *
 * The numbers did not change; what they claim did. Every count is still on
 * screen — `noParser + parseFailed + notProcessed` sums to `unindexed` — and
 * only the last two raise the card's tone.
 *
 * GUI §11 — every degraded state visible without navigating, so whatever tones
 * a card here also appears on that workspace's row in the sidebar.
 */

import { useCallback, useState } from "react";

import styles from "./StatusView.module.css";
import { cx } from "../lib/cx";
import { bytes, count, DASH, tilde } from "../lib/format";
import { StateBadge, type StateTone } from "./Badges";
import { ErrorNotice } from "./ErrorNotice";
import { Icon } from "./Icon";
import { useIndexHealth, useWorkspaces } from "../queries";
import { runIndex, unavailable } from "../actions";
import { addWorkspace, asUiError } from "../api";
import { useUi } from "../store";
import { useQueryClient } from "@tanstack/react-query";
import type { IndexHealth, WorkspaceRow } from "../api";

interface Verdict {
  tone: StateTone;
  word: string;
}

/**
 * The card's headline state.
 *
 * `noParser` is deliberately not here: a folder of photos is a healthy
 * workspace, and saying otherwise is the bug this page had.
 */
function verdict(w: WorkspaceRow): Verdict {
  if (w.files === 0) return { tone: "warn", word: "nothing indexed" };
  if (w.parseFailed > 0) return { tone: "warn", word: "parse failures" };
  if (w.notProcessed > 0) return { tone: "warn", word: "partly read" };
  if (w.cloudOnly > 0) return { tone: "warn", word: "partly on disk" };
  return { tone: "ok", word: "live" };
}

/**
 * The three facts this window still cannot know, and what would have to exist.
 *
 * Kept as data rather than as three `—` cells, because a dash that never
 * explains itself is indistinguishable from a bug. Each is one sentence and
 * names the command that is missing.
 */
const UNKNOWABLE: ReadonlyArray<{ k: string; why: string }> = [
  {
    k: "watcher",
    why: 'Whether the filesystem watcher is running needs a desktop command that does not exist yet ("watcher_state").',
  },
  {
    k: "last indexed",
    why: 'When this workspace was last indexed needs a desktop command that does not exist yet ("workspace_runs").',
  },
  {
    k: "last reconciled",
    why: 'When the index was last reconciled against the disk needs a desktop command that does not exist yet ("workspace_runs").',
  },
];

export function StatusView() {
  const workspaces = useWorkspaces();
  const health = useIndexHealth();
  const rows = workspaces.data ?? [];
  const h = health.data;
  const notify = useUi((s) => s.notify);
  const client = useQueryClient();
  const [adding, setAdding] = useState(false);

  // The picker is native and runs in Rust, so this awaits a real modal. A
  // cancel resolves `null`, which is a decision and not a failure.
  const add = useCallback(async () => {
    setAdding(true);
    try {
      const next = await addWorkspace();
      if (next) {
        client.setQueryData(["workspaces"], next);
        notify("Indexing it now. It becomes searchable as it goes.");
      }
    } catch (e) {
      notify(asUiError(e).message);
    } finally {
      setAdding(false);
    }
  }, [client, notify]);

  return (
    <div className={styles.view}>
      <div className={styles.scroll}>
        {workspaces.isError && (
          <ErrorNotice error={workspaces.error} action={null} />
        )}
        {health.isError && <ErrorNotice error={health.error} action={null} />}

        <div className={styles.actions}>
          <button
            type="button"
            className={styles.add}
            onClick={() => void add()}
            disabled={adding}
          >
            <Icon name="folder" size={13} />
            {adding ? "Choosing…" : "Add a folder"}
          </button>
        </div>

        {/* Above the counts, because it decides whether they mean anything. */}
        {h?.mayBeStale && <Freshness h={h} />}

        {rows.map((w) => {
          const v = verdict(w);
          return (
            <section
              key={w.name}
              className={cx(styles.card, v.tone === "warn" && styles.cardWarn)}
            >
              <header className={styles.cardHead}>
                <h2 className={styles.cardName}>{w.name}</h2>
                <span className={cx("mono", styles.cardPath)}>
                  {tilde(w.path)}
                </span>
                <span className={styles.grow} />
                <StateBadge tone={v.tone}>{v.word}</StateBadge>
              </header>

              {/* Real numbers, per workspace. Not one of them is a dash. */}
              <div className={styles.stats}>
                <Stat k="files" v={count(w.files)} />
                <Stat k="chunks" v={count(w.chunks)} />
                <Stat k="content" v={bytes(w.contentBytes)} />
                <Stat
                  k="cloud-only"
                  v={count(w.cloudOnly)}
                  tone={w.cloudOnly > 0 ? "warn" : undefined}
                />
              </div>

              <Unindexed w={w} />

              {/* What the backend cannot answer, said once, in words. */}
              <div className={styles.unknown}>
                {UNKNOWABLE.map((u) => (
                  <button
                    key={u.k}
                    type="button"
                    className={styles.unknownItem}
                    title={u.why}
                    onClick={() => notify(u.why)}
                  >
                    <span className={styles.unknownKey}>{u.k}</span>
                    <span className={cx("mono", styles.unknownValue)}>
                      {DASH}
                    </span>
                  </button>
                ))}
                <span className={styles.unknownNote}>
                  not exposed by this build
                </span>
              </div>

              {(w.files === 0 ||
                w.parseFailed > 0 ||
                w.notProcessed > 0 ||
                w.cloudOnly > 0) && (
                <div className={styles.issues}>
                  {w.files === 0 && (
                    <Issue
                      tone="warn"
                      title="Nothing in this workspace is indexed"
                      detail="The root is registered but no active files were found. An index run would populate it."
                      actions={[
                        {
                          label: "Run an index",
                          onClick: () => void runIndex(),
                        },
                      ]}
                    />
                  )}
                  {/* The only bucket where the text exists and Marrow does not
                      have it. `noParser` gets no issue at all — there is
                      nothing to fix about a photograph. */}
                  {w.parseFailed > 0 && (
                    <Issue
                      tone="warn"
                      title={`${count(w.parseFailed)} files could not be read in full`}
                      detail="A parser ran on these and came away with nothing searchable — corrupt, truncated, or a scan with no text layer. Unlike a file with no parser, the text is there and Marrow does not have it."
                      actions={[
                        {
                          label: "Retry parsing",
                          onClick: () => unavailable("retry"),
                        },
                      ]}
                    />
                  )}
                  {w.notProcessed > 0 && (
                    <Issue
                      tone="warn"
                      title={`${count(w.notProcessed)} files have not been read yet`}
                      detail="Nothing has opened their contents — an index run has not reached them, or was interrupted before it did. They are findable by name meanwhile."
                      actions={[
                        {
                          label: "Run an index",
                          onClick: () => void runIndex(),
                        },
                      ]}
                    />
                  )}
                  {w.cloudOnly > 0 && (
                    <Issue
                      tone="warn"
                      title={`${count(w.cloudOnly)} files are cloud-only and were not read`}
                      detail="Their metadata is indexed; their contents are not on this machine. Reading them is what triggers the download."
                      actions={[
                        {
                          label: "Keep as is",
                          onClick: () => unavailable("policy"),
                        },
                        {
                          label: "Download",
                          onClick: () => unavailable("hydrate"),
                        },
                      ]}
                    />
                  )}
                </div>
              )}
            </section>
          );
        })}

        {!workspaces.isError && rows.length === 0 && (
          <div className={styles.none}>
            <p>Marrow has not been given a folder yet.</p>
            <p className={styles.noneWhy}>
              It only ever reads folders you grant it, and it reads them on this
              machine — nothing is uploaded anywhere.
            </p>
          </div>
        )}
      </div>

      <footer className={styles.totals}>
        <Total v={count(rows.length)} k="workspaces" />
        <Total v={count(h?.files)} k="files" />
        <Total v={count(h?.chunks)} k="chunks" />
        <Total v={bytes(h?.contentBytes)} k="content" />
        <Total v={count(h?.cloudOnly)} k="cloud-only" />
        <span className={styles.grow} />
        <span className={cx("mono", styles.schema)}>
          {h === undefined
            ? DASH
            : h.mayBeStale
              ? "not watching"
              : `watching · ${h.watcher}`}
        </span>
        <span className={cx("mono", styles.schema)}>
          schema v{h === undefined ? DASH : h.schemaVersion}
        </span>
      </footer>
    </div>
  );
}

/**
 * Whether these numbers describe the disk, or describe it as it was.
 *
 * **A stale index is worse than no index.** No index answers nothing and the
 * user knows to scan; a stale one answers confidently about files it has not
 * looked at, and nothing else on this page would say so. Shown above the counts
 * because it decides whether the counts mean anything.
 *
 * Only rendered when something is actually wrong — a permanent banner saying
 * "everything is fine" is a banner people stop reading.
 */
function Freshness({ h }: { h: IndexHealth }) {
  const never = h.lastIndexedMs === null;
  return (
    <Issue
      tone="warn"
      title={
        never
          ? "These folders have never been scanned"
          : `Nothing is watching your folders — last checked ${ago(h.lastIndexedMs!)}`
      }
      detail={
        never
          ? "Nothing here reflects what is on your disk yet. A scan would populate it."
          : "Anything added, changed or deleted since then is not in the index, and a search cannot mention what it does not know about. The app watches while it is open; this usually means a folder became unwatchable."
      }
      actions={[{ label: "Run an index", onClick: () => void runIndex() }]}
    />
  );
}

/**
 * Which files have no searchable text, and why — the line this page got wrong.
 *
 * All four numbers, always, including the zeroes: a bucket that disappears when
 * empty makes "none of those" indistinguishable from "we stopped counting", and
 * "0 could not be read" is the most reassuring number on the card. Only the two
 * that a person can do something about are tinted.
 *
 * The parts sum to the total by construction in `catalog.rs`, so a reader can
 * check the arithmetic on screen and find it holds.
 */
function Unindexed({ w }: { w: WorkspaceRow }) {
  return (
    <div className={styles.breakdown}>
      <span className={styles.breakdownLead}>
        <span className={cx("mono", styles.breakdownTotal)}>
          {count(w.unindexed)}
        </span>{" "}
        of {count(w.files)} files have no searchable text
      </span>
      <span className={styles.breakdownParts}>
        <Part
          n={w.noParser}
          label="no parser"
          title="Photos, fonts, archives, binaries — nothing to extract text from. They stay findable by name, date and folder, which is what they are indexed for. Not a fault."
        />
        <Part
          n={w.parseFailed}
          label="could not be read"
          tone="warn"
          title="A parser ran and came away with nothing. This is the one worth acting on: the text exists and Marrow does not have it."
        />
        <Part
          n={w.notProcessed}
          label="not read yet"
          tone="warn"
          title="No parse has been attempted on these yet. An index run reaches them."
        />
      </span>
    </div>
  );
}

function Part({
  n,
  label,
  title,
  tone,
}: {
  n: number;
  label: string;
  title: string;
  tone?: "warn" | undefined;
}) {
  return (
    <span className={styles.part} title={title}>
      <span
        className={cx(
          "mono",
          styles.partValue,
          tone === "warn" && n > 0 && styles.statWarn,
        )}
      >
        {count(n)}
      </span>{" "}
      {label}
    </span>
  );
}

/** "3 minutes ago". Coarse on purpose: the decision is stale or not. */
function ago(ms: number): string {
  const s = Math.max(0, Math.round((Date.now() - ms) / 1000));
  if (s < 90) return "just now";
  if (s < 5_400) return `${Math.round(s / 60)} minutes ago`;
  if (s < 172_800) return `${Math.round(s / 3600)} hours ago`;
  return `${Math.round(s / 86_400)} days ago`;
}

function Stat({
  k,
  v,
  tone,
}: {
  k: string;
  v: string;
  tone?: "warn" | undefined;
}) {
  return (
    <div className={styles.stat}>
      <span
        className={cx(
          "mono",
          styles.statValue,
          tone === "warn" && styles.statWarn,
          v === DASH && styles.absent,
        )}
      >
        {v}
      </span>
      <span className={styles.statKey}>{k}</span>
    </div>
  );
}

function Total({ k, v }: { k: string; v: string }) {
  return (
    <span className={styles.total}>
      <span className={cx("mono", styles.totalValue)}>{v}</span> {k}
    </span>
  );
}

function Issue({
  tone,
  title,
  detail,
  actions,
}: {
  tone: StateTone;
  title: string;
  detail: string;
  actions: ReadonlyArray<{ label: string; onClick: () => void }>;
}) {
  return (
    <div className={styles.issue}>
      <Icon
        name="warning"
        size={14}
        className={tone === "error" ? styles.errorIcon : styles.warnIcon}
      />
      <div className={styles.issueBody}>
        <span className={styles.issueTitle}>{title}</span>
        <span className={styles.issueDetail}>{detail}</span>
      </div>
      <div className={styles.issueActions}>
        {actions.map((a) => (
          <button
            key={a.label}
            type="button"
            className={styles.issueAction}
            onClick={a.onClick}
          >
            {a.label}
          </button>
        ))}
      </div>
    </div>
  );
}
