/**
 * Settings.
 *
 * The nav item used to open nothing at all — it raised a toast saying settings
 * did not exist, which is worse than having no nav item: a control that visibly
 * refuses is still a control you have to try. This is a real view built out of
 * what is actually knowable.
 *
 * The rule it follows: **if a control has no command behind it, it is not
 * rendered as a control.** A greyed-out switch is a promise; a sentence is a
 * fact. Everything on the page is one of three things — a live control with a
 * command behind it (appearance, the answering endpoint, emptying the dropped
 * files), a number the core already returns, or a plainly-worded statement of
 * something this build cannot do.
 *
 * This paragraph used to read "exactly one live control", and stayed that way
 * through three additions that made it false. It is the page's own failure mode
 * in miniature, which is why the About group derives every claim it makes
 * instead of asserting one.
 *
 * One control breaks the rule and says so on screen rather than miming: the
 * name field has `prefs::set_user_name` behind it and no registered command in
 * front of it, so it reports itself unwired and names what is missing.
 *
 * **Four groups, one visible at a time.** Everything here was one continuous
 * scroll of eight headings in the order they happened to be built, which is an
 * order nobody chose. The split is `GROUPS` below; nothing was moved to another
 * view and nothing was dropped, and the group list is four ordinary buttons so
 * that Tab still reaches every one of them.
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";
import { getVersion } from "@tauri-apps/api/app";

import styles from "./SettingsView.module.css";
import { cx } from "../lib/cx";
import { bytes, count, DASH } from "../lib/format";
import { ErrorNotice } from "./ErrorNotice";
import { Icon } from "./Icon";
import { emptyScratch } from "../actions";
import {
  asUiError,
  clearCloudProvider,
  setCloudProvider,
  setUserName,
  userName,
} from "../api";
import {
  PROVIDER_KEY,
  useIndexHealth,
  useModels,
  useProviderSettings,
  useScratch,
  useWorkspaces,
} from "../queries";
import { useUi } from "../store";
import { resolveTheme, type ThemeChoice } from "../theme";

const THEMES: ReadonlyArray<{ id: ThemeChoice; label: string }> = [
  { id: "system", label: "System" },
  { id: "light", label: "Light" },
  { id: "dark", label: "Dark" },
];

/** Where the source is. Text, not a link — see the About section. */
const REPO = "https://github.com/CasualOffice/marrow";

/**
 * The app's own version, read out of the bundle.
 *
 * Not a constant in this file: a version maintained in two places is a version
 * that is wrong in one of them, and the About section exists to stop exactly
 * that. `getVersion` is part of `core:default`, which `capabilities/main.json`
 * already grants, so it costs no new permission and no new command.
 *
 * `null` is "not established" and renders as an em dash. A version is the sort
 * of thing it is tempting to fall back to a literal for, which would be the
 * two-places problem arriving through the error path.
 */
function useAppVersion(): string | null {
  const [version, setVersion] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const v =
          import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)
            ? (await import("../dev/fixtures")).APP_VERSION
            : await getVersion();
        if (live) setVersion(v);
      } catch {
        // Leave it unknown. There is nothing to retry and nothing to report:
        // a settings page that raises an error because it could not read its
        // own version number is worse than one that says it does not know.
      }
    })();
    return () => {
      live = false;
    };
  }, []);
  return version;
}

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

/**
 * The four groups, and what each one is for.
 *
 * Eight `<h2>`s in one scroll is a list, not an organisation: the order they
 * appeared in was the order they were built in, so a person looking for the
 * endpoint scrolled past the index counts to find it. Four groups is the whole
 * of the structure — deeper nesting would be inventing hierarchy for eight
 * sections.
 *
 * `blurb` is the group in one line, shown above its content. It is what makes
 * the split legible: without it, arriving in a group tells you what is in it
 * only by reading everything in it.
 */
type GroupId = "general" | "answering" | "index" | "about";

const GROUPS: ReadonlyArray<{ id: GroupId; label: string; blurb: string }> = [
  {
    id: "general",
    label: "General",
    blurb: "How Marrow addresses you, and how this window looks.",
  },
  {
    id: "answering",
    label: "Answering",
    blurb: "Which model writes answers, and where it runs.",
  },
  {
    id: "index",
    label: "Index",
    blurb: "What has been read, where it came from, and what it costs.",
  },
  {
    id: "about",
    label: "About",
    blurb: "What this is, what it can reach, and what it cannot do yet.",
  },
];

export function SettingsView() {
  // Local, deliberately. There is no route and no store entry for this, so a
  // group cannot be linked to or restored after a relaunch — which is the
  // right trade for a four-item switch inside one view. `useState` survives
  // the re-renders the queries below cause when they refetch on their timers;
  // only unmounting the view resets it, and that is the same thing every other
  // view's local state does.
  const [group, setGroup] = useState<GroupId>("general");
  const scroll = useRef<HTMLDivElement>(null);
  // `find` is total here — `group` only ever holds an id from `GROUPS` — but
  // `noUncheckedIndexedAccess` is on and a fallback that cannot fire is
  // cheaper than the assertion that says so.
  const current = GROUPS.find((g) => g.id === group) ?? GROUPS[0];
  const blurb = current?.blurb ?? "";

  // A group starts at its own top. Arriving at a short group from halfway down
  // a long one otherwise lands on blank space below its content.
  useEffect(() => {
    scroll.current?.scrollTo({ top: 0 });
  }, [group]);

  return (
    <div className={styles.view}>
      <div className={styles.columns}>
        {/* Plain buttons in a `<nav>`, not a tablist. A tablist is arguably
            the more precise role, but it owns the arrow keys and reduces the
            group list to a single tab stop — and Tab was only just released
            back to this page (UI_AUDIT §2). Four buttons are four tab stops
            and need no key handler at all, which is the behaviour that cannot
            regress. */}
        <nav className={styles.nav} aria-label="Settings sections">
          {GROUPS.map((g) => (
            <button
              key={g.id}
              type="button"
              className={cx(styles.navItem, group === g.id && styles.navItemOn)}
              aria-current={group === g.id ? "true" : undefined}
              onClick={() => setGroup(g.id)}
            >
              {g.label}
            </button>
          ))}
        </nav>

        <div className={styles.scroll} ref={scroll}>
          <div className={styles.measure}>
            <p className={styles.groupBlurb}>{blurb}</p>

            {group === "general" && (
              <>
                <YourName />
                <Appearance />
              </>
            )}

            {group === "answering" && <AnsweringEndpoint />}

            {group === "index" && (
              <>
                <IndexFacts />
                <WhereItLives />
                <DroppedFiles />
              </>
            )}

            {group === "about" && (
              <>
                <About />
                <CannotDoYet />
              </>
            )}
          </div>
        </div>
      </div>
    </div>
  );
}

/** The one control that is genuinely local to this window. */
function Appearance() {
  const theme = useUi((s) => s.theme);
  const setTheme = useUi((s) => s.setTheme);
  const resolved = resolveTheme(theme);

  return (
    <section className={styles.section}>
      <h2 className={styles.heading}>Appearance</h2>
      <p className={styles.lede}>
        Stored in this window and applied immediately. It is not part of the
        index and does not travel with it.
      </p>

      <div className={styles.segmented} role="radiogroup" aria-label="Appearance">
        {THEMES.map((t) => (
          <button
            key={t.id}
            type="button"
            role="radio"
            aria-checked={theme === t.id}
            className={cx(styles.segment, theme === t.id && styles.segmentOn)}
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
  );
}

/** What the index holds. The same numbers the Status view charts over time. */
function IndexFacts() {
  const health = useIndexHealth();
  const rows = useWorkspaces().data ?? [];
  const h = health.data;

  return (
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
  );
}

/** The granted roots, and the privacy posture in one paragraph. */
function WhereItLives() {
  const rows = useWorkspaces().data ?? [];
  // Read rather than assumed: the claim about where answers go is conditional
  // on this.
  const remote = useModels().data?.remote;

  return (
    <section className={styles.section}>
      <h2 className={styles.heading}>Where it lives</h2>
      <p className={styles.lede}>
        {/* This paragraph opened "Everything Marrow knows is on this
            machine", flat. That was true of every build until an answering
            endpoint could be configured, and it is the first thing a reader
            consults about the privacy posture — which makes it the worst
            sentence in the app to leave standing after it stops being true.
            The index half is still unconditional; the answering half is now
            read off what is actually set. */}
        Everything Marrow has indexed is on this machine, and the window itself
        is granted no filesystem, shell or network permission at all (SEC-012):
        the named commands it can call are the entire surface between it and the
        disk. Some of them do change things — granting a folder, downloading a
        model, starting an index run — and each one says so where it is
        offered.{" "}
        {remote?.enabled ? (
          <>
            <strong>Answers do not stay here.</strong> {remote.label} at{" "}
            <span className="mono">{remote.baseUrl}</span> generates them, so
            the question and the excerpts it cites are sent there.
          </>
        ) : (
          "Answers are generated on this machine, by a local model."
        )}
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
        The index database sits beside the application's own data directory.
        This window is not told the path, so it does not print a guess.
      </p>
    </section>
  );
}

/** The missing commands, named rather than mimed as dead switches. */
function CannotDoYet() {
  return (
    <section className={styles.section}>
      <h2 className={styles.heading}>What this build cannot do yet</h2>
      <p className={styles.lede}>
        Each of these would need a command that does not exist. They are listed
        rather than rendered as switches that do nothing.
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
  );
}

/**
 * What to call you.
 *
 * The only thing Marrow stores about the *person* rather than about their
 * files, and it is one text box for the reason `prefs::user_name` gives: the OS
 * account name, the Git author on the repositories in a workspace and the names
 * inside the indexed files would each produce a plausible answer, and the third
 * is not merely unreliable — a name read out of indexed content is a derived
 * fact about a person, which needs evidence, provenance and a correction path,
 * and which would then sit in a preferences file that records none of the
 * three. Asking is one field. Guessing is a subsystem that is confident and
 * sometimes wrong about somebody's name.
 *
 * **Inert in the app, live in `pnpm dev`.** `prefs::set_user_name` is written
 * and no registered command reaches it, so the failure is shown and the missing
 * command named — the same treatment "What this build cannot do yet", under
 * About, gives everything else that has no command behind it.
 */
type NameState =
  | { kind: "loading" }
  | { kind: "ready"; saved: string | null }
  | { kind: "unwired"; why: string };

function YourName() {
  const [state, setState] = useState<NameState>({ kind: "loading" });
  const [draft, setDraft] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let live = true;
    void (async () => {
      try {
        const saved = await userName();
        if (!live) return;
        setState({ kind: "ready", saved });
        setDraft(saved ?? "");
      } catch (e) {
        if (live) setState({ kind: "unwired", why: asUiError(e).message });
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  const save = async () => {
    setBusy(true);
    try {
      // Trimmed to `null` rather than sent as `""`: blank is how an answer is
      // withdrawn, and `prefs::set_user_name` normalises it the same way at
      // the other end so neither side is the only guard.
      const trimmed = draft.trim();
      const saved = await setUserName(trimmed === "" ? null : trimmed);
      setState({ kind: "ready", saved });
      setDraft(saved ?? "");
    } catch (e) {
      setState({ kind: "unwired", why: asUiError(e).message });
    } finally {
      setBusy(false);
    }
  };

  const unwired = state.kind === "unwired";
  const saved = state.kind === "ready" ? state.saved : null;
  const changed = draft.trim() !== (saved ?? "");

  return (
    <section className={styles.section}>
      <h2 className={styles.heading}>Your name</h2>
      <p className={styles.lede}>
        For addressing you and nothing else. It is not an account, nothing is
        signed in, and no part of the index, the search or the answering path
        reads it. Marrow does not work it out from your login, your Git commits
        or the names in your files — a name taken out of your own documents
        would be a guess with no provenance, stored as though it were a fact.
      </p>

      <div className={styles.nameRow}>
        <label className={cx(styles.field, styles.nameField)}>
          <span className={styles.fieldLabel}>what to call you</span>
          <input
            className={styles.input}
            value={draft}
            disabled={unwired || state.kind === "loading"}
            autoComplete="off"
            spellCheck={false}
            placeholder="left empty, and nothing greets you"
            onChange={(e) => setDraft(e.currentTarget.value)}
            onKeyDown={(e) => {
              if (e.key === "Enter" && changed && !busy) void save();
            }}
          />
        </label>
        <div className={styles.fieldSave}>
          <div className={styles.segmented}>
            <button
              type="button"
              className={cx(styles.segment, changed && styles.segmentOn)}
              disabled={unwired || busy || !changed}
              onClick={() => void save()}
            >
              {busy ? "Saving…" : "Save"}
            </button>
          </div>
        </div>
      </div>

      {unwired ? (
        <p className={styles.notice}>
          <Icon name="warning" size={12} />
          Not wired up yet, so the box above is inert.{" "}
          <span className="mono">preferences.json</span> has the field and{" "}
          <span className="mono">prefs::set_user_name</span> writes it; what is
          missing is a command this window can call —{" "}
          <span className="mono">set_user_name</span> — and the{" "}
          <span className="mono">user_name</span> that reads it back. The core
          reported: {state.why}
        </p>
      ) : (
        <p className={styles.hint}>
          {saved
            ? `Stored in preferences.json as "${saved}", alongside your other choices, and nowhere else.`
            : "Nothing stored, so nothing addresses you by name. Clearing the box puts it back to this."}
        </p>
      )}
    </section>
  );
}

/**
 * About Marrow — the same paragraph the model already gets.
 *
 * `Hub::identity` (`models.rs`) writes a block telling the *model* what it is,
 * where it runs and whether anything it is handed leaves the device. There was
 * no equivalent for the person, so the model was better briefed on the system
 * than its user was, and the asymmetry is the whole reason this section exists.
 *
 * **Every claim here is derived, none is asserted.** Nine claims in this
 * product were written as flat truths and became false the moment a remote
 * endpoint could be configured — including the one in `identity` itself, which
 * would have had a cloud model saying "nothing is sent to any external service"
 * in its own voice from somebody else's datacentre. The last claim below is
 * that claim again in a third place, so it is read off the provider status this
 * page already holds and is never written unconditionally. The rest are counts
 * and folder names from the same queries the other groups use.
 */
function About() {
  const health = useIndexHealth().data;
  const rows = useWorkspaces().data ?? [];
  const models = useModels().data;
  const remote = models?.remote;
  const version = useAppVersion();

  return (
    <section className={styles.section}>
      <h2 className={styles.heading}>About Marrow</h2>
      <p className={styles.lede}>
        Marrow indexes folders you grant it, works out how the files inside them
        are structured, and answers questions with a citation to the exact page,
        cell or line the answer came from. The same index is served over MCP, so
        an agent you already use can search it too. The citation is the point:{" "}
        <span className="mono">ripgrep</span> finds strings and a model with a
        folder mounted reads a handful of files and guesses, and neither can
        tell you where an answer came from or notice when the source changed.
      </p>
      <p className={styles.lede}>
        One author, built in the open, Apache-2.0. A personal project rather
        than a product: no account, no sign-in, nothing reported anywhere, and
        no promise that any of it keeps working.
      </p>

      <dl className={styles.facts}>
        <Fact k="version" v={version ?? DASH} />
        <Fact
          k="index schema"
          v={health ? `v${health.schemaVersion}` : DASH}
        />
        <Fact k="licence" v="Apache-2.0" />
        {/* The Models page's own words for this machine, not a second
            phrasing of the same probe. Two descriptions of one machine is two
            things to keep true. Last, because it takes the whole row and would
            otherwise strand the fact after it on a row of its own. */}
        <Fact k="this machine" v={models?.machine ?? DASH} wide />
      </dl>
      <p className={styles.hint}>
        Source: <span className="mono">{REPO}</span> — text rather than a link,
        because the WebView is granted no browser and no shell (SEC-012), and a
        link that does nothing when clicked is the failure this page is written
        to avoid.
      </p>

      <h3 className={styles.subheading}>What it can reach</h3>
      <ul className={styles.grants}>
        {rows.map((w) => (
          <li key={w.name} className={styles.grant}>
            <Icon name="folder" size={12} className={styles.rootIcon} />
            <span className={styles.grantName}>{w.name}</span>
            <span className={cx("mono", styles.grantCount)}>
              {count(w.files)} files
            </span>
          </li>
        ))}
        {rows.length === 0 && (
          <li className={styles.rootNone}>
            Nothing — no folder has been granted, so there is nothing to search.
          </li>
        )}
      </ul>

      <dl className={styles.claims}>
        <div className={styles.claim}>
          <dt className={styles.claimKey}>and nowhere else</dt>
          <dd className={styles.claimText}>
            Those folders are the whole of it. No other part of the disk is
            read, and this window reads none of it directly — it can only call
            the commands Marrow exposes to it.
          </dd>
        </div>
        <div className={styles.claim}>
          <dt className={styles.claimKey}>cloud-only files</dt>
          <dd className={styles.claimText}>
            {count(health?.cloudOnly)} files inside those folders live in
            iCloud, OneDrive or Dropbox and not on this disk. Marrow records
            their names and dates and never opens one, because opening a
            placeholder is what downloads it — that is your bandwidth and your
            disk, and it is not a decision this app makes for you.
          </dd>
        </div>
        <div className={styles.claim}>
          <dt className={styles.claimKey}>its own writing</dt>
          <dd className={styles.claimText}>
            Anything Marrow wrote itself is marked as such. It stays findable by
            search — hiding it would be its own kind of lie — but it can never
            be cited as evidence, so an answer can never corroborate itself with
            something it produced earlier.
          </dd>
        </div>
        <div className={styles.claim}>
          <dt className={styles.claimKey}>search</dt>
          <dd className={styles.claimText}>
            Works with no model, no GPU and no network, and is meant to keep
            working that way. Matching on meaning is added on top of matching on
            words; it never replaces it.
          </dd>
        </div>
        <div className={styles.claim}>
          <dt className={styles.claimKey}>answers</dt>
          {/* Read from `models.remote`, exactly as "Where it lives" above
              does. Neither branch is the safe one to assume. */}
          <dd className={styles.claimText}>
            {remote?.enabled ? (
              <>
                <strong>Leave this machine.</strong> {remote.label} at{" "}
                <span className="mono">{remote.baseUrl}</span> writes them, so
                the question and every excerpt it cites are sent there, under
                your own agreement with that provider. Turning it off under
                Answering stops it.
              </>
            ) : (
              <>
                Are written here, by a local model, and no question, file or
                excerpt is sent anywhere. Configuring an endpoint under
                Answering changes that, and this line changes with it.
              </>
            )}
          </dd>
        </div>
      </dl>

      <h3 className={styles.subheading}>What it will not do</h3>
      <p className={styles.lede}>
        Refusals, not a roadmap. Index your whole OS without asking · take
        destructive actions on its own · record your screen · recognise faces or
        voices · sync across devices · ship a mobile app · replace Git or
        filesystem permissions · treat an embedding as truth.
      </p>
    </section>
  );
}

/**
 * The answering endpoint (Part 8 §140).
 *
 * One endpoint, OpenAI-compatible, brought by the user. Four things this card
 * has to get right, and each of them is a requirement rather than a
 * preference:
 *
 * - **The key never comes back.** It is written to the OS keychain by the
 *   command that receives it, and no command returns it (LLM-030). The field
 *   below is write-only and empty on every render; "a key is saved" is the
 *   most this window is ever told.
 * - **The agreement is stated where the key is entered** (LLM-031). Not in a
 *   footnote and not on first use — at the field, because that is the moment
 *   the decision is made.
 * - **The boundary is resolved, not typed** (UX-012). `http://localhost:1234`
 *   and `api.openai.com` are not the same decision, and the difference is
 *   decided by the address the endpoint resolves to, with the addresses shown
 *   so the claim is checkable.
 * - **A classification that forbids it says so here**, rather than at the
 *   bottom of a failed answer (LLM-032).
 */
/** Both surfaces that show the endpoint: this card, and every claim the
 *  Models page and the Ask view make about where answers go. */
async function refresh(client: ReturnType<typeof useQueryClient>) {
  await Promise.all([
    client.invalidateQueries({ queryKey: PROVIDER_KEY }),
    client.invalidateQueries({ queryKey: ["models"] }),
  ]);
}

function AnsweringEndpoint() {
  const client = useQueryClient();
  const status = useProviderSettings().data;
  const [draft, setDraft] = useState<{
    label: string;
    baseUrl: string;
    model: string;
    key: string;
    maxOutputTokens: string;
    reasoningEffort: string;
  } | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // The form opens on what is configured. `draft === null` is "not editing",
  // which is a different state from "editing an empty form" — the second is
  // what a user gets after pressing Add.
  const editing = draft !== null;
  const open = (from?: typeof status) =>
    setDraft({
      label: from?.label ?? "",
      baseUrl: from?.baseUrl ?? "",
      model: from?.model ?? "",
      key: "",
      maxOutputTokens: String(from?.maxOutputTokens || 2048),
      reasoningEffort: from?.reasoningEffort ?? "",
    });

  const save = async (enabled: boolean) => {
    if (!draft) return;
    setBusy(true);
    setError(null);
    try {
      await setCloudProvider({
        enabled,
        label: draft.label,
        baseUrl: draft.baseUrl,
        model: draft.model,
        maxOutputTokens: Number(draft.maxOutputTokens) || 2048,
        reasoningEffort: draft.reasoningEffort || null,
        // `null` leaves whatever is in the keychain alone, so editing the
        // model name does not mean re-typing a key nobody can read back.
        key: draft.key === "" ? null : draft.key,
      });
      setDraft(null);
      await refresh(client);
    } catch (e) {
      setError(asUiError(e).message);
    } finally {
      setBusy(false);
    }
  };

  const toggle = async (enabled: boolean) => {
    if (!status?.configured) return;
    setBusy(true);
    setError(null);
    try {
      await setCloudProvider({
        enabled,
        label: status.label,
        baseUrl: status.baseUrl,
        model: status.model,
        maxOutputTokens: status.maxOutputTokens,
        reasoningEffort: status.reasoningEffort,
        key: null,
      });
      await refresh(client);
    } catch (e) {
      setError(asUiError(e).message);
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    setBusy(true);
    setError(null);
    try {
      await clearCloudProvider();
      setDraft(null);
      await refresh(client);
    } catch (e) {
      setError(asUiError(e).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className={styles.section}>
      <h2 className={styles.heading}>Answering</h2>
      <p className={styles.lede}>
        Answers are written by a local model unless you point Marrow at an
        OpenAI-compatible endpoint — your own server, or a provider you have an
        account with. Search never uses one: it works with no model and no
        network, and always will.
      </p>

      {status?.configured && !editing && (
        <div className={styles.form}>
          <dl className={styles.facts}>
            <Fact k="provider" v={status.label} />
            <Fact k="model" v={status.model} />
            <Fact k="endpoint" v={status.baseUrl} />
            <Fact k="key" v={status.hasKey ? "in the keychain" : "none saved"} />
          </dl>
          {status.boundaryLabel && (
            <p className={styles.boundary}>
              {/* Only where there is something to notice: a warning triangle
                  beside "on your own server" would make the safe answer look
                  like the alarming one. */}
              {status.boundary === "cloud" && <Icon name="warning" size={12} />}
              {status.boundaryLabel}
              {status.addresses.length > 0 && (
                <span className={styles.boundaryAddrs}>
                  {status.addresses.join(", ")}
                </span>
              )}
            </p>
          )}
          {status.problem && (
            <p className={styles.notice}>
              <Icon name="warning" size={12} />
              {status.problem}
            </p>
          )}
          {status.blockedBy && (
            /* LLM-032, said before a question is asked rather than after one
               is refused. Not overridable, so nothing here offers a way past
               it. */
            <p className={styles.notice}>
              <Icon name="warning" size={12} />
              {status.blockedBy}
            </p>
          )}
          <div className={styles.actions}>
            <label className={styles.check}>
              <input
                type="checkbox"
                checked={status.enabled}
                disabled={busy}
                onChange={(e) => void toggle(e.currentTarget.checked)}
              />
              Use it for answers
            </label>
            <span className={styles.grow} />
            <div className={styles.segmented}>
              <button
                type="button"
                className={styles.segment}
                disabled={busy}
                onClick={() => open(status)}
              >
                Edit
              </button>
              <button
                type="button"
                className={styles.segment}
                disabled={busy}
                onClick={() => void remove()}
              >
                Remove
              </button>
            </div>
          </div>
        </div>
      )}

      {!status?.configured && !editing && (
        <div className={styles.actions}>
          <div className={styles.segmented}>
            <button
              type="button"
              className={styles.segment}
              onClick={() => open()}
            >
              Add an endpoint
            </button>
          </div>
        </div>
      )}

      {editing && draft && (
        <div className={styles.form}>
          <div className={styles.fieldRow}>
            <label className={styles.field}>
              <span className={styles.fieldLabel}>name</span>
              <input
                className={styles.input}
                value={draft.label}
                placeholder="OpenAI"
                onChange={(e) =>
                  setDraft({ ...draft, label: e.currentTarget.value })
                }
              />
            </label>
            <label className={styles.field}>
              <span className={styles.fieldLabel}>model</span>
              <input
                className={cx("mono", styles.input)}
                value={draft.model}
                placeholder="gpt-4o-mini"
                onChange={(e) =>
                  setDraft({ ...draft, model: e.currentTarget.value })
                }
              />
            </label>
          </div>
          <label className={styles.field}>
            <span className={styles.fieldLabel}>endpoint</span>
            <input
              className={cx("mono", styles.input)}
              value={draft.baseUrl}
              placeholder="https://api.openai.com/v1"
              onChange={(e) =>
                setDraft({ ...draft, baseUrl: e.currentTarget.value })
              }
            />
            <span className={styles.hint}>
              The base, up to but not including <span className="mono">
              /chat/completions</span> — usually ending in <span className="mono">
              /v1</span>. Plain <span className="mono">http</span> is accepted
              only for a server on this machine or your own network.
            </span>
          </label>
          <label className={styles.field}>
            <span className={styles.fieldLabel}>key</span>
            <input
              className={styles.input}
              type="password"
              value={draft.key}
              autoComplete="off"
              spellCheck={false}
              placeholder={
                status?.hasKey ? "a key is saved — type to replace it" : ""
              }
              onChange={(e) =>
                setDraft({ ...draft, key: e.currentTarget.value })
              }
            />
            {/* LLM-031 · DPA-001. At the field, in plain words, because this
                is the moment the decision is made. */}
            <span className={styles.hint}>
              The key goes into your macOS keychain and nowhere else — not into
              a settings file, not into the index, not into a log. Marrow never
              proxies anything: requests go from this machine straight to the
              endpoint. <strong>Whatever you sent is governed by your own
              agreement with that provider</strong>, including what they keep
              and for how long. A server you run yourself needs no key.
            </span>
          </label>
          <div className={styles.fieldRow}>
            <label className={styles.field}>
              <span className={styles.fieldLabel}>longest answer (tokens)</span>
              <input
                className={styles.input}
                inputMode="numeric"
                value={draft.maxOutputTokens}
                onChange={(e) =>
                  setDraft({ ...draft, maxOutputTokens: e.currentTarget.value })
                }
              />
            </label>
            <label className={styles.field}>
              <span className={styles.fieldLabel}>reasoning effort</span>
              <select
                className={styles.input}
                value={draft.reasoningEffort}
                onChange={(e) =>
                  setDraft({
                    ...draft,
                    reasoningEffort: e.currentTarget.value,
                  })
                }
              >
                <option value="">not supported</option>
                <option value="low">low</option>
                <option value="medium">medium</option>
                <option value="high">high</option>
              </select>
              <span className={styles.hint}>
                There is no portable way to ask an OpenAI-compatible endpoint to
                think first. Left at "not supported", Thorough is refused for
                this endpoint rather than quietly answered as Fast.
              </span>
            </label>
          </div>
          {error && (
            <p className={styles.notice}>
              <Icon name="warning" size={12} />
              {error}
            </p>
          )}
          <div className={styles.actions}>
            <div className={styles.segmented}>
              <button
                type="button"
                className={cx(styles.segment, styles.segmentOn)}
                disabled={busy}
                onClick={() => void save(status?.enabled ?? true)}
              >
                Save
              </button>
              <button
                type="button"
                className={styles.segment}
                disabled={busy}
                onClick={() => {
                  setDraft(null);
                  setError(null);
                }}
              >
                Cancel
              </button>
            </div>
          </div>
        </div>
      )}
    </section>
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

/**
 * One fact in a `.facts` grid.
 *
 * `wide` gives it the whole row and lets it wrap. Every other value here is a
 * number or a short word, and `.factValue` truncates with an ellipsis to keep
 * the columns even — which is right until the value is a sentence. The machine
 * summary is three clauses, and truncated it reads "17 GB unified · 10 …",
 * which is a fact with its answer cut off.
 */
function Fact({ k, v, wide }: { k: string; v: string; wide?: boolean }) {
  return (
    <div className={cx(styles.fact, wide && styles.factWide)}>
      <dt className={styles.factKey}>{k}</dt>
      <dd className={cx("mono", styles.factValue, v === DASH && styles.absent)}>
        {v}
      </dd>
    </div>
  );
}
