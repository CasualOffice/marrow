/**
 * Status — index health, per workspace and in total.
 *
 * `list_workspaces` returns `chunks`, `contentBytes`, `cloudOnly` and
 * `unindexed` *per workspace* now, so the four stats that used to render as
 * `—` are real numbers. The em dash is reserved for what the backend genuinely
 * cannot answer — watcher state, when the workspace was last indexed, when it
 * was last reconciled — and each of those says what would have to exist for it
 * to be a number, rather than sitting there as a decorative dash.
 *
 * "An empty cell reads as 'fine'; `—` reads as 'we looked, nothing there'"
 * (UX principle 5). The corollary this view has to honour: a dash where a
 * number is available reads as a broken page.
 *
 * GUI §11 — every degraded state visible without navigating. `unindexed > 0`
 * and `cloudOnly > 0` are degraded states, so both raise the card's tone here
 * and both appear on the workspace's own row in the sidebar.
 */

import styles from "./StatusView.module.css";
import { cx } from "../lib/cx";
import { bytes, count, DASH, tilde } from "../lib/format";
import { StateBadge, type StateTone } from "./Badges";
import { ErrorNotice } from "./ErrorNotice";
import { Icon } from "./Icon";
import { useIndexHealth, useWorkspaces } from "../queries";
import { unavailable } from "../actions";
import { useUi } from "../store";
import type { WorkspaceRow } from "../api";

interface Verdict {
  tone: StateTone;
  word: string;
}

function verdict(w: WorkspaceRow): Verdict {
  if (w.files === 0) return { tone: "warn", word: "nothing indexed" };
  if (w.unindexed > 0) return { tone: "warn", word: "partly indexed" };
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

  return (
    <div className={styles.view}>
      <div className={styles.scroll}>
        {workspaces.isError && (
          <ErrorNotice error={workspaces.error} action={null} />
        )}
        {health.isError && <ErrorNotice error={health.error} action={null} />}

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

              {/* Five real numbers, per workspace. Not one of them is a dash. */}
              <div className={styles.stats}>
                <Stat k="files" v={count(w.files)} />
                <Stat k="chunks" v={count(w.chunks)} />
                <Stat k="content" v={bytes(w.contentBytes)} />
                <Stat
                  k="unindexed"
                  v={count(w.unindexed)}
                  tone={w.unindexed > 0 ? "warn" : undefined}
                />
                <Stat
                  k="cloud-only"
                  v={count(w.cloudOnly)}
                  tone={w.cloudOnly > 0 ? "warn" : undefined}
                />
              </div>

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

              {(w.files === 0 || w.unindexed > 0 || w.cloudOnly > 0) && (
                <div className={styles.issues}>
                  {w.files === 0 && (
                    <Issue
                      tone="warn"
                      title="Nothing in this workspace is indexed"
                      detail="The root is registered but no active files were found. An index run would populate it."
                      actions={[
                        {
                          label: "Run an index",
                          onClick: () => unavailable("reindex"),
                        },
                      ]}
                    />
                  )}
                  {w.unindexed > 0 && (
                    <Issue
                      tone="warn"
                      title={`${count(w.unindexed)} files are recorded from metadata alone`}
                      detail="They are findable by name and date. Their contents were never read, so no search of their text can match them."
                      actions={[
                        {
                          label: "Retry parsing",
                          onClick: () => unavailable("retry"),
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
          <p className={styles.none}>
            No workspaces are registered, so there is nothing to report.
          </p>
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
          schema v{h === undefined ? DASH : h.schemaVersion}
        </span>
      </footer>
    </div>
  );
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
