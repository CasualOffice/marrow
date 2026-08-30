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

import styles from "./SettingsView.module.css";
import { cx } from "../lib/cx";
import { bytes, count, DASH } from "../lib/format";
import { ErrorNotice } from "./ErrorNotice";
import { Icon } from "./Icon";
import { useIndexHealth, useWorkspaces } from "../queries";
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
 * exposes eight commands, all read-only, and a test asserts the surface stays
 * that way in M1. Nothing below is speculation about a roadmap.
 */
const CANNOT: ReadonlyArray<{ what: string; why: string; cmd: string }> = [
  {
    what: "Remove a workspace",
    why: "Adding one works — the Status page has a button. Removing is different: it has to decide what happens to everything indexed under it, and a wrong answer there deletes work rather than a folder reference.",
    cmd: "workspace_remove",
  },
  {
    what: "Start or schedule an index run",
    why: "Indexing happens outside the window. This build can report what the index contains but cannot ask for more of it.",
    cmd: "index_run",
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

          {/* ── where it lives ────────────────────────────────────────────── */}
          <section className={styles.section}>
            <h2 className={styles.heading}>Where it lives</h2>
            <p className={styles.lede}>
              Everything Marrow knows is on this machine. Nothing is uploaded,
              and the window itself is granted no filesystem, shell or network
              permission at all (SEC-012) — the eight read-only commands it can
              call are the entire surface between it and the disk.
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
