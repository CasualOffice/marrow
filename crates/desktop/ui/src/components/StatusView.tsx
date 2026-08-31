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
 * **The same correction, applied to the buttons.** "Retry parsing", "Keep as
 * is" and "Download" all called `unavailable()`: three controls whose entire
 * behaviour was to explain that they have no behaviour. Two of them could not
 * be built as offered — a retry over unchanged bytes with the same parser gets
 * the same result, and hydrating a placeholder is refused by hard rule 3, not
 * unimplemented — and the third asked the user to confirm what was already
 * happening. All three are gone and their reasons are in the detail text, on
 * the principle this page was rewritten for: state the situation, and offer an
 * action only when there is one.
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
import { pickFiles, runIndex } from "../actions";
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
  // An empty scratch workspace is empty, not broken. The folder Marrow keeps
  // dropped files in is *meant* to be empty until something is dropped, and
  // after it is emptied — warning about the ordinary state of a thing is how
  // this page earned its rewrite once already.
  if (w.scratch && w.files === 0) return { tone: "ok", word: "empty" };
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
  const setSetupOpen = useUi((s) => s.setSetupOpen);
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
          {/* The keyboard half of dropping onto the window, and the only path
              to indexing a single file that does not involve moving it first. */}
          <button
            type="button"
            className={styles.add}
            onClick={() => void pickFiles()}
          >
            <Icon name="file" size={13} />
            Add files
          </button>
          {/*
            The way back into the first-run flow.
            The flow opens itself when there are no workspaces at all, and never
            otherwise — so someone who skipped the model step, or who wants to
            re-read what each step costs, needs a door. This is it, and it is an
            ordinary focusable button rather than a shortcut, because a
            shortcut that is the sole path to something is GUI §11 read
            backwards.
          */}
          <button
            type="button"
            className={styles.add}
            onClick={() => setSetupOpen(true)}
          >
            <Icon name="arrowRight" size={13} />
            Set up Marrow
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

              {((w.files === 0 && !w.scratch) ||
                w.parseFailed > 0 ||
                w.notProcessed > 0 ||
                w.cloudOnly > 0) && (
                <div className={styles.issues}>
                  {/* Not for the dropped-files folder: empty is what it is
                      supposed to be until something is dropped, and "run an
                      index" would not put anything in it. */}
                  {w.files === 0 && !w.scratch && (
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
                  {/*
                    No "Retry parsing" button, and that is the fix rather than
                    a gap. It called `unavailable()` — a button whose entire
                    behaviour was to explain that it does nothing — and the
                    command it wanted would not have helped either: a parse is
                    keyed on (file version, parser, parser version), so running
                    one again over unchanged bytes with the same parser
                    produces the same nothing. What re-reads these files is the
                    file changing or the parser improving, and both happen on
                    their own. Saying so is worth more than a button that
                    cannot act (BUGS B2).
                  */}
                  {w.parseFailed > 0 && (
                    <Issue
                      tone="warn"
                      title={`${count(w.parseFailed)} files could not be read in full`}
                      detail="A parser ran on these and came away with nothing searchable — corrupt, truncated, or a scan with no text layer. Unlike a file with no parser, the text is there and Marrow does not have it. Nothing here retries them: the same parser over the same bytes gets the same result, so they are re-read when the file changes or when a build ships a better parser for it."
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
                  {/*
                    Neither "Download" nor "Keep as is", and for two different
                    reasons.

                    **Download is not a missing feature; it is a refusal.**
                    Hard rule 3: Marrow never hydrates a cloud placeholder.
                    Opening one is what makes the sync client fetch it, and a
                    button here would fetch every one of them — hundreds of
                    gigabytes on the author's own disk — from a page the user
                    opened to look at counts. The audit (§5) settles this as a
                    decision rather than a preference, so the honest form is
                    the reason in words and no control. It used to be a button
                    calling `unavailable("hydrate")`, which read as "this will
                    work later".

                    **Keep as is was a button for the thing already
                    happening.** Not doing something needs no affordance.
                  */}
                  {w.cloudOnly > 0 && (
                    <Issue
                      tone="warn"
                      title={`${count(w.cloudOnly)} files are cloud-only and were not read`}
                      detail="Their metadata is indexed and they are findable by name, date and folder; their contents are not on this machine. Marrow will not download them — reading a placeholder is what makes your sync client fetch it, and doing that here would pull every one of them at once. Open one in Finder or its own app and it downloads; the next sweep then indexes its contents."
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

/**
 * A degraded state, and the button that clears it when there is one.
 *
 * `actions` is optional because two of the states on this page have no action
 * that can honestly be offered — a parse that will not change, and a
 * placeholder Marrow refuses to hydrate (hard rule 3). The empty container is
 * not rendered at all: a button that explains it does nothing is worse than no
 * button, and so is the gap where one used to be.
 */
function Issue({
  tone,
  title,
  detail,
  actions = [],
}: {
  tone: StateTone;
  title: string;
  detail: string;
  actions?: ReadonlyArray<{ label: string; onClick: () => void }>;
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
      {actions.length > 0 && (
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
      )}
    </div>
  );
}
