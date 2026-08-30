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
import { useQueryClient } from "@tanstack/react-query";

import styles from "./SettingsView.module.css";
import { cx } from "../lib/cx";
import { bytes, count, DASH } from "../lib/format";
import { ErrorNotice } from "./ErrorNotice";
import { Icon } from "./Icon";
import { emptyScratch } from "../actions";
import { asUiError, clearCloudProvider, setCloudProvider } from "../api";
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
  // Read rather than assumed: every claim below about where answers go is
  // conditional on this.
  const remote = useModels().data?.remote;

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

          {/* ── where answers are generated ───────────────────────────────── */}
          <AnsweringEndpoint />

          {/* ── where it lives ────────────────────────────────────────────── */}
          <section className={styles.section}>
            <h2 className={styles.heading}>Where it lives</h2>
            <p className={styles.lede}>
              {/* This paragraph opened "Everything Marrow knows is on this
                  machine", flat. That was true of every build until an
                  answering endpoint could be configured, and it is the first
                  thing a reader consults about the privacy posture — which
                  makes it the worst sentence in the app to leave standing
                  after it stops being true. The index half is still
                  unconditional; the answering half is now read off what is
                  actually set. */}
              Everything Marrow has indexed is on this machine, and the window
              itself is granted no filesystem, shell or network permission at
              all (SEC-012): the named commands it can call are the entire
              surface between it and the disk. Some of them do change things —
              granting a folder, downloading a model, starting an index run —
              and each one says so where it is offered.{" "}
              {remote?.enabled ? (
                <>
                  <strong>Answers do not stay here.</strong> {remote.label} at{" "}
                  <span className="mono">{remote.baseUrl}</span> generates them,
                  so the question and the excerpts it cites are sent there.
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
