/**
 * Settings.
 *
 * The nav item used to open nothing at all — it raised a toast saying settings
 * did not exist, which is worse than having no nav item: a control that visibly
 * refuses is still a control you have to try. This is a real view built out of
 * what is actually knowable.
 *
 * The rule it follows, and the reason it is short: **if a control has no
 * command behind it, it is not rendered as a control.** A greyed-out switch is
 * a promise; a sentence is a fact. So there is exactly one live control here —
 * appearance, which is genuinely local to this window — and everything else is
 * either a number the core already returns or a plainly-worded statement of
 * what this build cannot do.
 */

import { useCallback, useState } from "react";

import styles from "./SettingsView.module.css";
import { cx } from "../lib/cx";
import { bytes, count, DASH } from "../lib/format";
import { ErrorNotice } from "./ErrorNotice";
import { Icon } from "./Icon";
import { emptyScratch } from "../actions";
import { useIndexHealth, useScratch, useWorkspaces } from "../queries";
import { useUi } from "../store";
import { resolveTheme, type ThemeChoice } from "../theme";

const THEMES: ReadonlyArray<{ id: ThemeChoice; label: string }> = [
  { id: "system", label: "System" },
  { id: "light", label: "Light" },
  { id: "dark", label: "Dark" },
];

/**
 * What this build cannot do, in the user's terms first and the missing
 * command's terms second.
 *
 * Every line here is checkable against `crates/desktop/src/commands.rs`: it
 * exposes a fixed, named list, and a test pins its size. **It is no longer
 * read-only**: granting a folder writes the database and starts watchers,
 * downloading a model writes to disk and uses the network, and an index run
 * rewrites the index. The page used to say "eight read-only commands" when
 * there were thirty-one and several mutate — body copy a user reads to
 * understand the security posture, which makes it the worst place in the app to
 * be wrong. The guarantee that survives is the one SEC-012 actually makes: the
 * WebView has no ambient capability, only these named calls.
 *
 * Nothing below is speculation about a roadmap.
 */
const CANNOT: ReadonlyArray<{ what: string; why: string; cmd: string }> = [
  {
    what: "Remove a workspace",
    why: "Adding one works — the Status page has a button. Removing is different: it has to decide what happens to everything indexed under it, and a wrong answer there deletes work rather than a folder reference.",
    cmd: "workspace_remove",
  },
  {
    what: "Schedule an index run for later",
    why: "Starting one now works — the Status page has a button, and the app watches your folders while it is open. What does not exist is a schedule: nothing runs at a time you choose.",
    cmd: "index_schedule",
  },
  {
    what: "Change what a workspace indexes",
    why: "File-type filters, ignore rules and size caps are index policy, and policy is not editable from here.",
    cmd: "workspace_set_policy",
  },
  {
    what: "Download cloud-only files",
    why: "Their contents are not on this machine. Reading one is what triggers the download, which is a decision this window will not make on your behalf (invariant #5).",
    cmd: "workspace_hydrate",
  },
  {
    what: "Open a file at a line in $EDITOR",
    why: "⌘↵ hands the file to the system's default application, which is the open this build has. Jumping to a line in a specific editor needs a command that knows about editors.",
    cmd: "open_in_editor",
  },
  {
    what: "Retry a file that failed to parse",
    why: "Files recorded from metadata alone are listed on Status. Asking for another attempt is a write, and there are no writes.",
    cmd: "job_retry",
  },
  {
    what: "Register a global hotkey",
    why: "⌘K opens quick find inside the app. Summoning it while another app has focus needs a plugin the capability manifest does not grant.",
    cmd: "tauri-plugin-global-shortcut",
  },
];

export function SettingsView() {
  const theme = useUi((s) => s.theme);
  const setTheme = useUi((s) => s.setTheme);
  const health = useIndexHealth();
  const workspaces = useWorkspaces();
  const h = health.data;
  const rows = workspaces.data ?? [];
  const resolved = resolveTheme(theme);

  return (
    <div className={styles.view}>
      <div className={styles.scroll}>
        <div className={styles.measure}>
          {/* ── appearance: the one live control ──────────────────────────── */}
          <section className={styles.section}>
            <h2 className={styles.heading}>Appearance</h2>
            <p className={styles.lede}>
              Stored in this window and applied immediately. It is not part of
              the index and does not travel with it.
            </p>

            <div
              className={styles.segmented}
              role="radiogroup"
              aria-label="Appearance"
            >
              {THEMES.map((t) => (
                <button
                  key={t.id}
                  type="button"
                  role="radio"
                  aria-checked={theme === t.id}
                  className={cx(
                    styles.segment,
                    theme === t.id && styles.segmentOn,
                  )}
                  onClick={() => setTheme(t.id)}
                >
                  {t.label}
                </button>
              ))}
            </div>
            <p className={styles.hint}>
              {theme === "system"
                ? `Following the system, which is currently ${resolved}.`
                : `Fixed to ${theme}, whatever the system is set to.`}
            </p>
          </section>

          {/* ── the index ─────────────────────────────────────────────────── */}
          <section className={styles.section}>
            <h2 className={styles.heading}>Index</h2>
            {health.isError ? (
              <ErrorNotice error={health.error} action={null} />
            ) : (
              <dl className={styles.facts}>
                <Fact k="schema version" v={h ? `v${h.schemaVersion}` : DASH} />
                <Fact k="files" v={count(h?.files)} />
                <Fact k="chunks" v={count(h?.chunks)} />
                <Fact k="content" v={bytes(h?.contentBytes)} />
                <Fact k="cloud-only" v={count(h?.cloudOnly)} />
                <Fact k="workspaces" v={count(rows.length)} />
              </dl>
            )}
          </section>

          {/* ── dropped files ─────────────────────────────────────────────── */}
          <DroppedFiles />

          {/* ── where it lives ────────────────────────────────────────────── */}
          <section className={styles.section}>
            <h2 className={styles.heading}>Where it lives</h2>
            <p className={styles.lede}>
              Everything Marrow knows is on this machine, and the window itself
              is granted no filesystem, shell or network permission at all
              (SEC-012): the named commands it can call are the entire surface
              between it and the disk. Some of them do change things — granting a
              folder, downloading a model, starting an index run — and each one
              says so where it is offered.
            </p>
            <ul className={styles.roots}>
              {rows.map((w) => (
                <li key={w.name} className={styles.root}>
                  <Icon name="folder" size={13} className={styles.rootIcon} />
                  <span className={styles.rootName}>{w.name}</span>
                  <span className={cx("mono", styles.rootPath)}>{w.path}</span>
                </li>
              ))}
              {rows.length === 0 && (
                <li className={styles.rootNone}>No workspaces are registered.</li>
              )}
            </ul>
            <p className={styles.hint}>
              The index database sits beside the application's own data
              directory. This window is not told the path, so it does not print
              a guess.
            </p>
          </section>

          {/* ── what this build cannot do ─────────────────────────────────── */}
          <section className={styles.section}>
            <h2 className={styles.heading}>What this build cannot do yet</h2>
            <p className={styles.lede}>
              Each of these would need a command that does not exist. They are
              listed rather than rendered as switches that do nothing.
            </p>
            <ul className={styles.cannot}>
              {CANNOT.map((c) => (
                <li key={c.cmd} className={styles.cannotItem}>
                  <span className={styles.cannotWhat}>{c.what}</span>
                  <span className={styles.cannotWhy}>{c.why}</span>
                  <span className={cx("mono", styles.cannotCmd)}>{c.cmd}</span>
                </li>
              ))}
            </ul>
          </section>
        </div>
      </div>
    </div>
  );
}

/**
 * The dropped-files folder: what is in it, what it costs, and how to empty it.
 *
 * "Temporary" needed a definition, and this card is half of it. Marrow's answer
 * is **until you throw it away, under a ceiling** — not per session, because
 * conversations persist and a conversation is the one thing in that database
 * which cannot be re-derived from your files. Deleting the evidence a saved
 * answer cites, on quit, would rot every thread that was ever answered from a
 * dropped file, silently and a launch later.
 *
 * The other half is the ceiling, stated here rather than discovered when an
 * eviction happens. Oldest copies go first, and the notice names them.
 *
 * The numbers come from the **disk**, not from the index: the question this
 * card answers is what the duplication is costing, and a file copied in a
 * moment ago is costing it whether or not a sweep has reached it.
 */
function DroppedFiles() {
  const scratch = useScratch();
  const [busy, setBusy] = useState(false);
  const s = scratch.data;

  // `emptyScratch` refreshes every panel a change to the index touches — a
  // cleared folder is a changed workspace, file list, count and ranking, and
  // this card is only one of the five.
  const empty = useCallback(async () => {
    setBusy(true);
    try {
      await emptyScratch();
    } finally {
      setBusy(false);
    }
  }, []);

  return (
    <section className={styles.section}>
      <h2 className={styles.heading}>Dropped files</h2>
      <p className={styles.lede}>
        Files dropped on the window, or added with ⌘O, are <em>copied</em> into a
        folder Marrow owns and indexed there. Copied rather than referenced
        because a file left where it was could be moved or deleted with nothing
        watching it, and the index would go on citing a path that is not there.
        Your originals are never moved or changed.
      </p>
      <p className={styles.lede}>
        They stay until you empty this — not until you quit, because an answer
        you saved last week still cites them. It holds up to{" "}
        {bytes(s?.maxBytes)} in total and {bytes(s?.maxFileBytes)} per file;
        past that, the oldest copies are removed to make room and you are told
        which.
      </p>

      {scratch.isError ? (
        <ErrorNotice error={scratch.error} action={null} />
      ) : (
        <dl className={styles.facts}>
          <Fact k="files" v={s ? count(s.files) : DASH} />
          <Fact k="on disk" v={s ? bytes(s.bytes) : DASH} />
          <Fact
            k="folder"
            v={s === undefined ? DASH : (s.path ?? "not created yet")}
          />
        </dl>
      )}

      <div className={styles.segmented}>
        <button
          type="button"
          className={styles.segment}
          disabled={busy || s === undefined || s.files === 0}
          onClick={() => void empty()}
        >
          <Icon name="trash" size={12} />
          {busy ? "Emptying…" : "Empty it"}
        </button>
      </div>
      <p className={styles.hint}>
        {s !== undefined && s.files === 0
          ? "Nothing is in it, so there is nothing to remove."
          : "Removes Marrow's copies and takes them out of the index. Nothing you wrote is touched."}
      </p>
    </section>
  );
}

function Fact({ k, v }: { k: string; v: string }) {
  return (
    <div className={styles.fact}>
      <dt className={styles.factKey}>{k}</dt>
      <dd className={cx("mono", styles.factValue, v === DASH && styles.absent)}>
        {v}
      </dd>
    </div>
  );
}
