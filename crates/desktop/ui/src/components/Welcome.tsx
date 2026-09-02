/**
 * First run — the path from "installed" to "working".
 *
 * **There wasn't one.** Opening Marrow for the first time produced a search
 * box over nothing: no folder granted, no model, no indication that either was
 * a thing you were supposed to do. Every affordance that would have helped
 * existed — `add_workspace` on the Status page, the Models page, `reindex` —
 * and none of them was where a new user was standing.
 *
 * ── It decides from real state, never from a flag ─────────────────────────
 *
 * There is no "has seen the welcome" bit anywhere, in `localStorage` or in the
 * database, and adding one would be the bug rather than the fix. A flag is a
 * claim about the user that goes stale: it is wrong for someone who deletes
 * every workspace and is back to an empty window with no way forward, and it is
 * wrong the other way for someone restoring a machine from backup with an index
 * already in place. The question this asks instead — *are there any workspaces?*
 * — is answered by the index itself, so the help arrives exactly when the window
 * is empty and never when it is not.
 *
 * The one exception is `setupOpen`, which records that the *user* said
 * something ("skip", or "set up" on the Status page). That is a fact about this
 * run of the app rather than a claim about their history, and it is not
 * persisted for the same reason.
 *
 * ── Any workspace counts, including the dropped-files one ─────────────────
 *
 * Someone who drops a PDF on the window has found a path from installed to
 * working, and that is the whole test. Counting only *granted* folders would
 * put this dialog in front of them at every launch for as long as they preferred
 * dropping to granting — a nag, which is what a "seen it" flag is usually
 * introduced to fix.
 *
 * ── It is honest about what each step costs ───────────────────────────────
 *
 * Search needs no model, no GPU and no network, and never will (hard rule 10).
 * Step 3 says so before it offers a download, and the size it quotes is the
 * real `downloadBytes` for the model that would actually be fetched rather than
 * a number typed into a string. Someone who declines is not in a degraded
 * state, and this must not imply that they are.
 */

import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useQueryClient } from "@tanstack/react-query";

import styles from "./Welcome.module.css";
import { cx } from "../lib/cx";
import { bytes, count } from "../lib/format";
import { Icon } from "./Icon";
import { Kbd } from "./Kbd";
import { grantFolder, pickFiles } from "../actions";
import { asUiError, downloadModel, type ModelRow, type ModelsSnapshot } from "../api";
import { RuntimeSetup } from "./ModelsView";
import { useIndexHealth, useModels, useWorkspaces } from "../queries";
import { useUi } from "../store";

/**
 * Whether the guided setup is on screen.
 *
 * Called by `App` and by nothing else, for two reasons that both bit:
 *
 *   * `App` has to know, because the dialog owns the keyboard while it is up.
 *     A second copy of "is it open" would drift the first time either changed,
 *     and the failure looks like arrow keys moving an invisible selection.
 *   * The dialog is **mounted only while it is visible**. It reads the Models
 *     page's snapshot, which refetches every four seconds because the numbers
 *     on it are live — mounting it always would put that poll behind every
 *     screen in the app, for ever, to render nothing.
 *
 * **The derived answer latches.** The state is read once — *are there any
 * workspaces?* — and the moment it says "none", that becomes an explicit
 * `setupOpen: true` which only the user can undo. Re-deriving on every render
 * was the obvious version and it was wrong: granting a folder in step 1 makes
 * the condition false, so the dialog would delete itself the instant the user
 * did the thing it asked for, taking steps 2 and 3 with it.
 *
 * There is still no persisted flag. Latching lasts for this run of the app;
 * next launch the question is asked of the index again, so someone who deletes
 * everything gets help back and someone who is working never sees it.
 */
export function useSetupGate(): boolean {
  const decided = useUi((s) => s.setupOpen);
  const setSetupOpen = useUi((s) => s.setSetupOpen);
  const workspaces = useWorkspaces();
  // Not while the list is still loading. Flashing this in front of someone who
  // has ten workspaces, for the one frame before the query lands, is worse than
  // showing it a beat late to someone who has none.
  const empty = workspaces.data !== undefined && workspaces.data.length === 0;

  useEffect(() => {
    if (decided === null && empty) setSetupOpen(true);
  }, [decided, empty, setSetupOpen]);

  return decided === true;
}

/**
 * The model this machine should be offered, and the one it already has.
 *
 * Chosen through the tiering that already exists rather than by naming a model
 * here: `generator.paramsB` is what the selected profile says a machine like
 * this should run, so the offer is the downloadable model closest to it. A
 * hard-coded id would go stale the first time the catalogue moved, and a
 * "biggest that fits" rule would offer a 14B download to someone who has not
 * yet searched anything.
 *
 * Embedders are excluded on both counts: they make search match on meaning and
 * they cannot write an answer, so one being installed says nothing about
 * whether Ask will work.
 */
function generative(s: ModelsSnapshot): {
  installed: ModelRow | null;
  offer: ModelRow | null;
} {
  const rows = s.models.filter((m) => !m.capabilities.includes("embedding"));
  const installed = rows.find((m) => m.installed) ?? null;
  const want = s.generator.paramsB;
  const offer =
    [...rows]
      .filter((m) => !m.installed && m.downloadable)
      .sort((a, b) => Math.abs(a.paramsB - want) - Math.abs(b.paramsB - want))[0] ?? null;
  return { installed, offer };
}

/** Mounted only while it is open — see [`useSetupGate`]. */
export function Welcome() {
  const setSetupOpen = useUi((s) => s.setSetupOpen);
  const setView = useUi((s) => s.setView);
  const notify = useUi((s) => s.notify);
  const client = useQueryClient();

  const workspaces = useWorkspaces();
  const health = useIndexHealth();
  const models = useModels();

  const panel = useRef<HTMLDivElement>(null);
  const firstAction = useRef<HTMLButtonElement>(null);
  const [busy, setBusy] = useState(false);

  const close = useCallback(() => setSetupOpen(false), [setSetupOpen]);

  /* Focus the primary action on open, so the flow is usable without ever
     touching the mouse. */
  useEffect(() => {
    firstAction.current?.focus();
  }, []);

  const rows = useMemo(() => workspaces.data ?? [], [workspaces.data]);
  const granted = rows.filter((w) => !w.scratch);
  const hasSomething = rows.length > 0;
  const files = health.data?.files ?? 0;
  const chunks = health.data?.chunks ?? 0;
  const snapshot = models.data;
  const model = snapshot ? generative(snapshot) : null;

  const download = useCallback(
    async (id: string) => {
      setBusy(true);
      try {
        client.setQueryData(["models"], await downloadModel(id));
      } catch (e) {
        notify(asUiError(e).message);
      } finally {
        setBusy(false);
      }
    },
    [client, notify],
  );

  const search = useCallback(() => {
    close();
    setView("search");
    // The field takes focus on its own when the view mounts; this is the
    // navigation, not a second focus call fighting it.
  }, [close, setView]);

  return (
    <div className={styles.scrim}>
      <div
        ref={panel}
        className={styles.panel}
        role="dialog"
        aria-modal="true"
        aria-labelledby="setup-title"
        onKeyDown={(e) => {
          if (e.key === "Escape") {
            e.preventDefault();
            e.stopPropagation();
            close();
            return;
          }
          // Keep focus inside. Everything focusable here is a button, and there
          // is nothing behind the dialog worth tabbing to while it is up.
          if (e.key !== "Tab") return;
          const focusable = panel.current?.querySelectorAll<HTMLElement>(
            "button:not([disabled])",
          );
          if (!focusable || focusable.length === 0) return;
          const first = focusable[0];
          const last = focusable[focusable.length - 1];
          if (!first || !last) return;
          if (e.shiftKey && document.activeElement === first) {
            e.preventDefault();
            last.focus();
          } else if (!e.shiftKey && document.activeElement === last) {
            e.preventDefault();
            first.focus();
          }
        }}
      >
        <header className={styles.head}>
          <div>
            <h1 id="setup-title" className={styles.title}>
              Get Marrow reading
            </h1>
            <p className={styles.sub}>
              Three steps, and the third is optional. Everything happens on this
              machine.
            </p>
          </div>
          <span className={styles.grow} />
          <button type="button" className={styles.skip} onClick={close}>
            Skip <Kbd>Esc</Kbd>
          </button>
        </header>

        <ol className={styles.steps}>
          {/* ── 1 ─────────────────────────────────────────────────────────── */}
          <Step
            n={1}
            title="Give it something to read"
            done={hasSomething}
            state={
              hasSomething
                ? granted.length > 0
                  ? `${granted.map((w) => w.name).join(", ")} granted`
                  : "files dropped in"
                : null
            }
          >
            <p className={styles.lede}>
              Marrow reads only what you hand it, and it reads it here — nothing
              is uploaded anywhere. A folder is indexed where it already lives;
              individual files are copied into a folder Marrow owns, which you
              can empty at any time.
            </p>
            <div className={styles.actions}>
              <button
                ref={firstAction}
                type="button"
                className={styles.primary}
                onClick={() => void grantFolder()}
              >
                <Icon name="folder" size={13} />
                Choose a folder
                <Kbd label="Shift Command O">⇧⌘O</Kbd>
              </button>
              <button
                type="button"
                className={styles.secondary}
                onClick={() => void pickFiles()}
              >
                <Icon name="file" size={13} />
                Add files
                <Kbd label="Command O">⌘O</Kbd>
              </button>
            </div>
            <p className={styles.aside}>
              Or drop files straight onto this window — including onto this
              dialog.
            </p>
          </Step>

          {/* ── 2 ─────────────────────────────────────────────────────────── */}
          <Step
            n={2}
            title="It indexes while you watch"
            done={files > 0}
            state={files > 0 ? `${count(files)} files · ${count(chunks)} chunks` : null}
            dimmed={!hasSomething}
          >
            {!hasSomething ? (
              <p className={styles.lede}>
                Nothing to index yet. This fills in on its own once step 1 is
                done.
              </p>
            ) : files === 0 ? (
              <>
                <p className={styles.lede}>
                  Reading your files now. This is the first pass; it becomes
                  searchable as it goes rather than at the end.
                </p>
                {/* SKEL-001: a skeleton in the shape of the counts that are
                    coming, not a spinner. There is no honest percentage to
                    show — nothing knows how many files are down there until
                    the walk has been down there. */}
                <div className={styles.skeleton} aria-busy="true">
                  <span className="srOnly">Indexing…</span>
                  <div className={cx(styles.skel, styles.skelStat)} />
                  <div className={cx(styles.skel, styles.skelStat)} />
                </div>
              </>
            ) : (
              <>
                <p className={styles.lede}>
                  Already searchable. Marrow keeps watching these folders while
                  it is open, so this stays true without you running anything.
                </p>
                <div className={styles.actions}>
                  <button type="button" className={styles.primary} onClick={search}>
                    <Icon name="search" size={13} />
                    Try a search
                    <Kbd label="Command F">⌘F</Kbd>
                  </button>
                </div>
              </>
            )}
          </Step>

          {/* ── 3 ─────────────────────────────────────────────────────────── */}
          <Step
            n={3}
            title="Answers, if you want them"
            optional
            done={model?.installed != null}
            state={model?.installed ? `${model.installed.displayName} installed` : null}
          >
            <p className={styles.lede}>
              <strong className={styles.strong}>
                Search never needs a model.
              </strong>{" "}
              It matches words in your files, on this machine, with no GPU and no
              network, and that does not change. A model is what turns a search
              into a written answer with citations — it runs locally too, and it
              has to be downloaded once.
            </p>
            <ModelStep
              snapshot={snapshot}
              model={model}
              busy={busy}
              onDownload={download}
              onModels={() => {
                close();
                setView("models");
              }}
            />
          </Step>
        </ol>

        <footer className={styles.foot}>
          <p className={styles.footNote}>
            You can reopen this from Status at any time, and the Models page has
            the full account of what will run here.
          </p>
          <span className={styles.grow} />
          <button type="button" className={styles.done} onClick={close}>
            {hasSomething ? "Done" : "Not now"}
          </button>
        </footer>
      </div>
    </div>
  );
}

/**
 * The model half of step 3.
 *
 * Split out because it has five states that read very differently and folding
 * them into the parent's JSX made the honest ones easy to lose: installed,
 * downloading, offerable, blocked, and no runtime at all. The last two are the
 * ones a first-run flow is tempted to hide, and hiding them produces "why is
 * there no download button" with no way to answer it (LLM-016).
 */
function ModelStep({
  snapshot,
  model,
  busy,
  onDownload,
  onModels,
}: {
  snapshot: ModelsSnapshot | undefined;
  model: { installed: ModelRow | null; offer: ModelRow | null } | null;
  busy: boolean;
  onDownload: (id: string) => void;
  onModels: () => void;
}) {
  if (!snapshot || !model) {
    return (
      <div className={styles.skeleton} aria-busy="true">
        <span className="srOnly">Reading this machine…</span>
        <div className={cx(styles.skel, styles.skelStat)} />
      </div>
    );
  }

  // An endpoint answers instead, so none of the five states below apply — and
  // three of them ("answers are not available here", "nothing here can answer
  // a question yet") would be false. The first-run flow must not send someone
  // to download three gigabytes they have already decided not to use.
  if (snapshot.remote.enabled) {
    return (
      <>
        <p className={styles.ready}>
          <Icon name="chip" size={13} />
          {snapshot.remote.label} answers questions
          {snapshot.remote.boundaryLabel
            ? ` — ${snapshot.remote.boundaryLabel}`
            : ""}
          . Settings has the switch, and a local model can be downloaded here
          as well.
        </p>
        <div className={styles.actions}>
          <button type="button" className={styles.secondary} onClick={onModels}>
            See the Models page
          </button>
        </div>
      </>
    );
  }

  if (model.installed) {
    return (
      <>
        <p className={styles.ready}>
          <Icon name="chip" size={13} />
          {model.installed.displayName} is installed. Ask is ready.
        </p>
        <div className={styles.actions}>
          <button type="button" className={styles.secondary} onClick={onModels}>
            See the Models page
          </button>
        </div>
      </>
    );
  }

  const offer = model.offer;
  const progress = offer?.progress ?? null;

  if (progress && progress.stage.stage !== "failed" && progress.stage.stage !== "cancelled") {
    const pct =
      progress.bytesTotal > 0 ? (progress.bytesDone / progress.bytesTotal) * 100 : 0;
    return (
      <div className={styles.progress}>
        {/* Determinate, so the width is the truth and not a guess (SKEL-005). */}
        <div
          className={styles.bar}
          role="progressbar"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(pct)}
          aria-label="Download progress"
        >
          <div className={styles.barFill} style={{ width: `${pct}%` }} />
        </div>
        <p className={styles.progressLine}>
          <span className={cx("mono", styles.progressNums)}>
            {bytes(progress.bytesDone)} of {bytes(progress.bytesTotal)}
          </span>
          <span className={styles.grow} />
          <button type="button" className={styles.secondary} onClick={onModels}>
            Watch it on Models
          </button>
        </p>
        <p className={styles.aside}>
          You do not have to wait here. Search already works, and closing this
          does not stop the transfer.
        </p>
      </div>
    );
  }

  if (!snapshot.runtimeReady) {
    // Never a button that cannot work — and for four releases this screen had
    // the opposite problem: no button at all, and a printed instruction whose
    // first line macOS could not run. This is where a fresh install lands, so
    // this is where the fix has to be reachable.
    return (
      <>
        <p className={styles.blocked}>
          <Icon name="warning" size={13} />
          {snapshot.runtimeStatus}
        </p>
        {snapshot.runtimeSetup && (
          <RuntimeSetup
            installable={snapshot.runtimeInstallable}
            downloadBytes={snapshot.runtimeDownloadBytes}
            install={snapshot.runtimeInstall}
            setup={snapshot.runtimeSetup}
          />
        )}
        <p className={styles.aside}>
          None of this affects search, which is already working.
        </p>
      </>
    );
  }

  if (!offer) {
    return (
      <p className={styles.blocked}>
        <Icon name="warning" size={13} />
        Nothing in the catalogue can run on this machine, so answers are not
        available here. The Models page shows each one with the arithmetic.
      </p>
    );
  }

  return (
    <>
      <div className={styles.actions}>
        <button
          type="button"
          className={styles.primary}
          disabled={busy}
          onClick={() => onDownload(offer.id)}
        >
          <Icon name="chip" size={13} />
          Download {offer.displayName} · {bytes(offer.downloadBytes)}
        </button>
        <button type="button" className={styles.secondary} onClick={onModels}>
          Compare models
        </button>
      </div>
      {/* The size, the licence and what it needs to run — before the download,
          not after it (LIC-004, LLM-016). */}
      <p className={styles.aside}>
        {offer.fitReason} {offer.licence}.
      </p>
    </>
  );
}

function Step({
  n,
  title,
  children,
  done,
  state,
  optional,
  dimmed,
}: {
  n: number;
  title: string;
  children: ReactNode;
  done: boolean;
  state: string | null;
  optional?: boolean;
  dimmed?: boolean;
}) {
  return (
    <li className={cx(styles.step, done && styles.stepDone, dimmed && styles.stepDim)}>
      <span className={cx("mono", styles.marker)} aria-hidden="true">
        {done ? <Icon name="arrowRight" size={12} /> : n}
      </span>
      <div className={styles.stepBody}>
        <h2 className={styles.stepTitle}>
          {title}
          {optional && <span className={styles.optional}>optional</span>}
          {/* The state is a word, never only the tick: colour and a glyph are
              never the sole carrier of meaning (A11Y-003). */}
          {state !== null && <span className={styles.stepState}>{state}</span>}
        </h2>
        {children}
      </div>
    </li>
  );
}
