/**
 * Zero results is a diagnosis, not a shrug (UX §4, GUI §5.3).
 *
 * It shows what is *not* indexed and offers the fix as a button, because "the
 * state that caused the problem is the state that can fix it". Every number
 * here comes from `index_health` or `list_workspaces` — nothing is estimated,
 * and a gap with no data behind it is not shown at all.
 *
 * The gap buttons call commands that do not exist yet; `unavailable()` says so
 * by name rather than doing nothing. The "Or try" chips are entirely local and
 * work today.
 */

import styles from "./ZeroResults.module.css";
import { cx } from "../lib/cx";
import { count, ms } from "../lib/format";
import { grantFolder, runIndex } from "../actions";
import { useIndexHealth, useWorkspaces } from "../queries";
import type { UiError } from "../api";
import { ErrorNotice } from "./ErrorNotice";

interface Gap {
  id: string;
  n: string;
  tone: "warn" | "dim";
  title: string;
  detail: string;
  /** Optional: a gap whose honest answer is a reason, not a control. */
  action?: string;
  onClick?: () => void;
}

/**
 * Narrower queries that the index can actually answer differently. Derived from
 * the query text alone, so every chip is one click from a real search.
 */
function alternatives(query: string): string[] {
  const words = query.trim().split(/\s+/).filter(Boolean);
  const out: string[] = [];
  if (words.length > 1) {
    out.push(words.slice(0, -1).join(" "));
    out.push(words.slice(1).join(" "));
    const longest = [...words].sort((a, b) => b.length - a.length)[0];
    if (longest && !out.includes(longest)) out.push(longest);
  }
  const stripped = query.replace(/[^\p{L}\p{N}\s]/gu, " ").replace(/\s+/g, " ").trim();
  if (stripped !== query.trim() && stripped !== "") out.push(stripped);
  return [...new Set(out)].filter((a) => a !== query.trim()).slice(0, 4);
}

export interface ZeroResultsProps {
  query: string;
  elapsedMs: number;
  error: UiError | null;
  onTry: (q: string) => void;
}

export function ZeroResults({
  query,
  elapsedMs,
  error,
  onTry,
}: ZeroResultsProps) {
  const health = useIndexHealth();
  const workspaces = useWorkspaces();

  const files = health.data?.files;
  const spaces = workspaces.data?.length;
  const cloudOnly = health.data?.cloudOnly ?? 0;

  // Files with no searchable text, which is not a fault — a photo has no words
  // — but is the reason a search can come back empty over a folder that is
  // fully indexed.
  const unreadable = (workspaces.data ?? []).reduce(
    (n, w) => n + w.noParser + w.parseFailed + w.notProcessed,
    0,
  );

  const gaps: Gap[] = [];

  if (spaces === 0) {
    gaps.push({
      id: "no-workspaces",
      n: "0",
      tone: "warn",
      title: "workspaces have been added",
      detail: "Nothing is indexed yet, so nothing can match.",
      action: "Add a folder",
      // Was `unavailable("policy")` — a notice about `workspace_set_policy`, a
      // different feature entirely, on the button of the page a first-run user
      // sees. `add_workspace` has existed since the Status page got it.
      onClick: () => void grantFolder(),
    });
  }

  // A registered root with nothing in it is the most concrete gap there is.
  for (const w of workspaces.data ?? []) {
    if (w.files === 0) {
      gaps.push({
        id: `empty-${w.name}`,
        n: "0",
        tone: "warn",
        title: `files are indexed in ${w.name}`,
        detail: `The root is registered at ${w.path} but nothing in it has been read.`,
        action: "Run an index",
        // Also `unavailable("policy")`, also the wrong feature's message.
        onClick: () => void runIndex(),
      });
    }
  }

  if (cloudOnly > 0) {
    gaps.push({
      id: "cloud-only",
      n: count(cloudOnly),
      tone: "warn",
      title: "cloud-only files were not read",
      // No button, and the reason instead. Hard rule 3: reading a placeholder
      // is what makes the sync client fetch it, so a "Download" here would
      // pull every one of them at once — from a screen the user opened
      // because a search found nothing. StatusView says the same thing in the
      // same words; this was the last place still offering the button and
      // calling `unavailable("hydrate")`, which reads as "this will work
      // later" rather than "this is refused".
      detail:
        "Their metadata is indexed and they are findable by name, date and folder; their contents are not on this machine. Marrow will not download them — reading a placeholder is what makes your sync client fetch it. Open one in Finder or its own app and it downloads; the next sweep indexes its contents.",
    });
  }

  const searched =
    files === undefined || spaces === undefined
      ? null
      : `Searched ${count(files)} ${files === 1 ? "file" : "files"} across ${count(
          spaces,
        )} ${spaces === 1 ? "workspace" : "workspaces"} in ${ms(elapsedMs)}.`;

  const alts = alternatives(query);

  return (
    <div className={styles.wrap}>
      <div className={styles.panel}>
        <div className={styles.headline}>
          <h1 className={styles.title}>No matches for “{query}”</h1>
          {searched !== null && <p className={styles.sub}>{searched}</p>}
        </div>

        {error !== null && <ErrorNotice error={error} action={null} />}

        <div className={styles.rule} />

        {gaps.length > 0 ? (
          <section className={styles.section}>
            <h2 className={styles.heading}>Not indexed — it may be here</h2>
            {gaps.map((g) => (
              <div key={g.id} className={styles.gap}>
                <span
                  className={cx(
                    "mono",
                    styles.gapCount,
                    g.tone === "warn" && styles.warn,
                  )}
                >
                  {g.n}
                </span>
                <div className={styles.gapBody}>
                  <span className={styles.gapTitle}>{g.title}</span>
                  <span className={styles.gapDetail}>{g.detail}</span>
                </div>
                {/* Only when there is one. A gap whose honest answer is a
                    reason renders no control, rather than a button that
                    explains it does nothing. */}
                {g.action && g.onClick && (
                  <button
                    type="button"
                    className={styles.gapAction}
                    onClick={g.onClick}
                  >
                    {g.action}
                  </button>
                )}
              </div>
            ))}
          </section>
        ) : (
          <section className={styles.section}>
            <h2 className={styles.heading}>
              {unreadable > 0
                ? "Some files have no searchable text"
                : "Nothing is missing from the index"}
            </h2>
            {/*
              This used to say flatly "Every file in every workspace was read",
              shown whenever three narrow checks passed — which is the ordinary
              state. It was false on any real corpus: most files have no chunks,
              and the count saying so arrives on the same query this component
              already makes and never looked at.
            */}
            <p className={styles.sub}>
              {unreadable > 0
                ? `${count(unreadable)} indexed ${unreadable === 1 ? "file has" : "files have"} no text to match — mostly images and binaries, which stay findable by name. These words are not in the rest.`
                : "Every file in every workspace was read. These words are not in any of them."}
            </p>
          </section>
        )}

        {alts.length > 0 && (
          <section className={styles.section}>
            <h2 className={styles.heading}>Or try</h2>
            <div className={styles.chips}>
              {alts.map((a) => (
                <button
                  key={a}
                  type="button"
                  className={styles.chip}
                  onClick={() => onTry(a)}
                >
                  {a}
                </button>
              ))}
            </div>
          </section>
        )}
      </div>
    </div>
  );
}
