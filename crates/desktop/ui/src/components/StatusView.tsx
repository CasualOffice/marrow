/**
 * Status — index health, per workspace and in total (design/Status.dc.html).
 *
 * The mockup's cards carry five per-workspace stats and per-workspace issues.
 * `list_workspaces` returns three fields and `index_health` is global, so the
 * stats this window cannot know render as `—` rather than as a plausible
 * number: "an empty cell reads as 'fine'; `—` reads as 'we looked, nothing
 * there'" (UX principle 5). The gaps are listed in the accompanying report.
 */

import styles from "./StatusView.module.css";
import { cx } from "../lib/cx";
import { bytes, count, DASH, tilde } from "../lib/format";
import { StateBadge, type StateTone } from "./Badges";
import { ErrorNotice } from "./ErrorNotice";
import { Icon } from "./Icon";
import { useIndexHealth, useWorkspaces } from "../queries";
import { unavailable } from "../actions";

export function StatusView() {
  const workspaces = useWorkspaces();
  const health = useIndexHealth();
  const rows = workspaces.data ?? [];
  const h = health.data;

  return (
    <div className={styles.view}>
      <div className={styles.scroll}>
        {workspaces.isError && (
          <ErrorNotice error={workspaces.error} action={null} />
        )}
        {health.isError && <ErrorNotice error={health.error} action={null} />}

        {rows.map((w) => {
          const tone: StateTone = w.files === 0 ? "warn" : "ok";
          const state = w.files === 0 ? "nothing indexed" : "live";
          return (
            <section
              key={w.name}
              className={cx(styles.card, tone === "warn" && styles.cardWarn)}
            >
              <header className={styles.cardHead}>
                <h2 className={styles.cardName}>{w.name}</h2>
                <span className={cx("mono", styles.cardPath)}>
                  {tilde(w.path)}
                </span>
                <span className={styles.grow} />
                <StateBadge tone={tone}>{state}</StateBadge>
              </header>

              <div className={styles.stats}>
                <Stat k="files" v={count(w.files)} />
                <Stat k="content" v={DASH} />
                <Stat k="chunks" v={DASH} />
                <Stat k="indexed" v={DASH} />
                <Stat k="reconciled" v={DASH} />
              </div>

              {w.files === 0 && (
                <div className={styles.issues}>
                  <Issue
                    tone="warn"
                    title="Nothing in this workspace is indexed"
                    detail="The root is registered but no active files were found. An index run would populate it."
                    actions={[
                      { label: "Run an index", onClick: () => unavailable("policy") },
                    ]}
                  />
                </div>
              )}
            </section>
          );
        })}

        {/* Cloud-only is a whole-index number, so it is a whole-index issue.
            Shown even at zero elsewhere; shown as an issue only above zero. */}
        {h !== undefined && h.cloudOnly > 0 && (
          <section className={cx(styles.card, styles.cardWarn)}>
            <header className={styles.cardHead}>
              <h2 className={styles.cardName}>Cloud-only files</h2>
              <span className={styles.grow} />
              <StateBadge tone="warn">metadata only</StateBadge>
            </header>
            <div className={styles.issues}>
              <Issue
                tone="warn"
                title={`${count(h.cloudOnly)} files are cloud-only and were not read`}
                detail="Their metadata is indexed; their contents are not on this machine. Reading them is what triggers the download."
                actions={[
                  { label: "Keep as is", onClick: () => unavailable("policy") },
                  { label: "Download", onClick: () => unavailable("hydrate") },
                ]}
              />
            </div>
          </section>
        )}
      </div>

      <footer className={styles.totals}>
        <Total v={count(rows.length)} k="workspaces" />
        <Total v={count(h?.files)} k="files" />
        <Total v={bytes(h?.contentBytes)} k="content" />
        <Total v={count(h?.chunks)} k="chunks" />
        <Total v={count(h?.cloudOnly)} k="cloud-only" />
        <span className={styles.grow} />
        <span className={styles.schema}>
          schema v{h === undefined ? DASH : h.schemaVersion}
        </span>
      </footer>
    </div>
  );
}

function Stat({ k, v }: { k: string; v: string }) {
  return (
    <div className={styles.stat}>
      <span className={cx("mono", styles.statValue, v === DASH && styles.absent)}>
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
