/**
 * Models — what this machine can run, and what is stopping each one.
 *
 * The rule that shapes every row (LLM-016): **a model that will not fit is
 * shown, disabled, with the arithmetic.** Hiding it produces "why isn't Llama
 * 70B here?" and no way to answer. The corollary this view has to honour is
 * the Settings bug in a different costume — a download button that cannot
 * work is worse than no button, so a blocked row says what is blocking it.
 *
 * Every number here is live. "This machine" is the probe's own words; free
 * memory comes from the sampler, not the launch probe, because a
 * recommendation made at launch is wrong by the time it is acted on (LLM-019).
 */

import { useCallback, useEffect, useRef, useState } from "react";

import styles from "./ModelsView.module.css";
import { cx } from "../lib/cx";
import { bytes } from "../lib/format";
import { StateBadge, type StateTone } from "./Badges";
import { ErrorNotice } from "./ErrorNotice";
import { Icon } from "./Icon";
import { useModels } from "../queries";
import {
  asUiError,
  cancelModelDownload,
  dismissModelDownload,
  downloadModel,
  setGeneratorModel,
  type ModelsSnapshot,
  refreshModelDetection,
  setAiProfile,
  startSemanticBackfill,
  stopSemanticBackfill,
  type DownloadProgress,
  type ModelRow,
  type ModelState,
  type SemanticStatus,
} from "../api";
import { useQueryClient } from "@tanstack/react-query";

/** The word beside a row, and its tone. Both, never one (A11Y-003). */
function fitVerdict(m: ModelRow): { tone: StateTone; word: string } {
  if (m.installed) return { tone: "ok", word: "installed" };
  switch (m.fit) {
    case "comfortable":
      return { tone: "ok", word: "fits" };
    case "tight":
      return { tone: "warn", word: "tight" };
    case "too_large":
      return { tone: "error", word: "too large" };
  }
}

/** `4212` seconds reads as nothing; `1h 10m` reads as an answer. */
function duration(secs: number): string {
  if (secs < 60) return `${Math.max(1, Math.round(secs))}s`;
  if (secs < 3600) return `${Math.round(secs / 60)}m`;
  const h = Math.floor(secs / 3600);
  return `${h}h ${Math.round((secs - h * 3600) / 60)}m`;
}

/**
 * SKEL-005: real bytes and a real ETA, never an indeterminate bar.
 *
 * The stage names the file, so a transfer that stalls is attributable to
 * something rather than to "downloading".
 */
function DownloadBar({
  p,
  onCancel,
  onDismiss,
}: {
  p: DownloadProgress;
  onCancel: () => void;
  onDismiss: () => void;
}) {
  const pct = p.bytesTotal > 0 ? (p.bytesDone / p.bytesTotal) * 100 : 0;
  const done = p.stage.stage === "ready";
  const failed = p.stage.stage === "failed";
  const stopped = failed || p.stage.stage === "cancelled";

  let line: string;
  switch (p.stage.stage) {
    case "downloading":
      line = `${p.stage.file} · file ${p.stage.index} of ${p.stage.of}`;
      break;
    case "verifying":
      line = `checking ${p.stage.file}`;
      break;
    case "ready":
      line = "ready";
      break;
    case "cancelled":
      line = "cancelled — what was fetched is kept, so starting again resumes";
      break;
    case "failed":
      line = p.stage.reason;
      break;
  }

  return (
    <div className={styles.progress}>
      <div
        className={styles.bar}
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={Math.round(pct)}
        aria-label="Download progress"
      >
        <div
          className={cx(styles.barFill, done && styles.barDone, stopped && styles.barStopped)}
          style={{ width: `${done ? 100 : pct}%` }}
        />
      </div>
      <div className={styles.progressLine}>
        <span className={failed ? styles.progressFailed : styles.progressStage}>{line}</span>
        <span className={styles.grow} />
        {!stopped && !done && (
          <>
            <span className={styles.progressNums}>
              {bytes(p.bytesDone)} of {bytes(p.bytesTotal)}
            </span>
            {p.bytesPerSec > 0 && (
              <span className={styles.progressNums}>{bytes(p.bytesPerSec)}/s</span>
            )}
            {/* No ETA until there is a rate worth dividing by. An ETA
                invented from one chunk is worse than none. */}
            {p.etaSecs !== null && (
              <span className={styles.progressNums}>{duration(p.etaSecs)} left</span>
            )}
            <button type="button" className={styles.linkBtn} onClick={onCancel}>
              Cancel
            </button>
          </>
        )}
        {stopped && (
          <button type="button" className={styles.linkBtn} onClick={onDismiss}>
            Dismiss
          </button>
        )}
      </div>
    </div>
  );
}

/** `32768` reads as nothing; `32k context` reads as a fact. */
function contextWindow(m: ModelRow): string {
  const k = Math.round(m.contextLimit / 1024);
  return `${k}k context`;
}

function stateWord(s: ModelState): string {
  switch (s.state) {
    case "loading":
      return `loading · ${s.stage}`;
    case "suspended":
      return "suspended";
    default:
      return s.state;
  }
}

/**
 * Licence facts sit on the row, before anything that downloads (LIC-004).
 *
 * `commercialUse === null` renders as "not established" rather than as either
 * yes or no. We have not checked, and saying so is the only honest option.
 */
function Licence({ m }: { m: ModelRow }) {
  const commercial =
    m.commercialUse === null
      ? "commercial use not established"
      : m.commercialUse
        ? "commercial use allowed"
        : "no commercial use";
  return (
    <span className={styles.licence}>
      {m.licenceUrl ? (
        <a href={m.licenceUrl} target="_blank" rel="noreferrer">
          {m.licence}
        </a>
      ) : (
        m.licence
      )}
      <span className={styles.licenceDot}>·</span>
      {commercial}
    </span>
  );
}

function ModelCard({
  m,
  onDownload,
  onCancel,
  onDismiss,
  busy,
}: {
  m: ModelRow;
  onDownload: (id: string) => void;
  onCancel: (id: string) => void;
  onDismiss: (id: string) => void;
  busy: boolean;
}) {
  const v = fitVerdict(m);
  const blocked = m.fit === "too_large" || m.blockedReason !== null;
  const blockedNote =
    m.blockedReason !== null && m.blockedReason !== m.fitReason ? m.blockedReason : null;
  return (
    <li className={cx(styles.card, blocked && !m.installed && styles.cardBlocked)}>
      <div className={styles.cardHead}>
        <span
          className={cx(
            styles.dot,
            m.installed ? styles.dotOn : m.fit === "too_large" ? styles.dotOff : styles.dotIdle,
          )}
          aria-hidden="true"
        />
        <h3 className={styles.name}>{m.displayName}</h3>
        <span className={styles.spec}>
          {m.paramsB.toFixed(m.paramsB < 10 ? 1 : 0)}B · {m.quantization} · {m.format}
        </span>
        <span className={styles.grow} />
        <span className={styles.size}>{bytes(m.requiredBytes)}</span>
        <StateBadge tone={v.tone}>{v.word}</StateBadge>
      </div>

      <p className={styles.role}>{m.role}</p>

      <p className={styles.fitReason}>
        {m.fitReason}
        {/* Only when the gap is large enough to be surprising, and phrased
            with the real factor — "many times this machine" is false for a
            model whose ceiling is merely twice its run context. */}
        {m.contextLimit >= m.runContext * 4 && (
          <span className={styles.ctxNote}>
            {" "}
            Sized at {Math.round(m.runContext / 1024)}k of the{" "}
            {Math.round(m.contextLimit / 1024)}k it supports; the full window
            would need about {Math.round(m.contextLimit / m.runContext)}× this
            model's cache.
          </span>
        )}
      </p>
      <p className={styles.breakdown}>{m.breakdown}</p>
      {m.repo && (
        <p className={styles.source}>
          <a
            href={`https://huggingface.co/${m.repo}`}
            target="_blank"
            rel="noreferrer"
          >
            {m.repo}
          </a>
          <span className={styles.licenceDot}>·</span>
          <span className={styles.mono}>{m.revisionShort}</span>
          <span className={styles.licenceDot}>·</span>
          {m.fileCount} files, {bytes(m.downloadBytes)}
          {!m.kvMeasured && (
            <>
              <span className={styles.licenceDot}>·</span>
              cache size estimated
            </>
          )}
        </p>
      )}

      <div className={styles.tags}>
        {m.capabilities.map((c) => (
          <span key={c} className={styles.tag}>
            {c}
          </span>
        ))}
        {m.detectedIn && <span className={styles.tagDetected}>in {m.detectedIn}</span>}
        <span className={styles.grow} />
        <Licence m={m} />
      </div>

      {m.reasoningUnavailable && (
        <p className={styles.note}>
          <Icon name="warning" size={11} />
          Thorough answers are unavailable — {m.reasoningUnavailable}
        </p>
      )}

      {m.suspendedReason && (
        <p className={styles.suspended}>
          <Icon name="warning" size={11} />
          {m.suspendedReason}
        </p>
      )}

      {m.progress && (
        <DownloadBar
          p={m.progress}
          onCancel={() => onCancel(m.id)}
          onDismiss={() => onDismiss(m.id)}
        />
      )}

      <div className={styles.actions}>
        {m.progress && m.progress.stage.stage !== "failed" &&
        m.progress.stage.stage !== "cancelled" ? null : m.installed ? (
          // The badge already says "installed", so repeating it here would be
          // two words for one fact. Show the lifecycle state only when it is
          // more than that, and the context window otherwise.
          <span className={styles.stateText}>
            {m.state.state === "installed" ? contextWindow(m) : stateWord(m.state)}
          </span>
        ) : m.downloadable ? (
          <button
            type="button"
            className={styles.get}
            disabled={busy}
            onClick={() => onDownload(m.id)}
          >
            {m.progress ? "Resume" : "Download"} {bytes(m.downloadBytes)}
          </button>
        ) : (
          // A too-large row's blocking reason *is* its fit reason, and the fit
          // reason is already above. Saying it twice reads as a rendering bug.
          blockedNote !== null && <p className={styles.blocked}>{blockedNote}</p>
        )}
      </div>
    </li>
  );
}

/** One tier of §139.5, so the memory design is visible rather than implied. */
function Role({
  title,
  r,
}: {
  title: string;
  r: { paramsB: number; resident: boolean; why: string };
}) {
  return (
    <div className={styles.role3}>
      <div className={styles.role3Head}>
        <span className={styles.role3Title}>{title}</span>
        <span className={styles.role3Size}>
          {r.paramsB === 0 ? "remote" : `~${r.paramsB}B`}
        </span>
      </div>
      <span className={cx(styles.role3Life, r.resident && styles.role3Resident)}>
        {r.resident ? "stays loaded" : "loads on demand"}
      </span>
      <p className={styles.role3Why}>{r.why}</p>
    </div>
  );
}

export function ModelsView() {
  const q = useModels();
  const client = useQueryClient();
  const [busy, setBusy] = useState(false);
  // Kept beside the page rather than thrown: a failed download must say why,
  // and the query itself succeeded.
  const [actionError, setActionError] = useState<ReturnType<typeof asUiError> | null>(null);

  const choose = useCallback(
    async (id: string) => {
      setBusy(true);
      try {
        const next = await setAiProfile(id);
        client.setQueryData(["models"], next);
      } finally {
        setBusy(false);
      }
    },
    [client],
  );

  const act = useCallback(
    async (fn: (id: string) => Promise<typeof s>, id: string) => {
      setBusy(true);
      try {
        client.setQueryData(["models"], await fn(id));
      } catch (e) {
        setActionError(asUiError(e));
      } finally {
        setBusy(false);
      }
    },
    [client],
  );

  const run = useCallback(
    async (fn: () => Promise<typeof s>) => {
      setBusy(true);
      try {
        client.setQueryData(["models"], await fn());
      } catch (e) {
        setActionError(asUiError(e));
      } finally {
        setBusy(false);
      }
    },
    [client],
  );

  const rescan = useCallback(async () => {
    setBusy(true);
    try {
      client.setQueryData(["models"], await refreshModelDetection());
    } finally {
      setBusy(false);
    }
  }, [client]);

  const s = q.data;

  return (
    <section className={styles.view} aria-label="Models">
      <div className={styles.scroll}>
        {q.error && <ErrorNotice error={asUiError(q.error)} action={null} />}
        {actionError && <ErrorNotice error={actionError} action={null} />}

        {!s ? (
          <SkeletonPage />
        ) : (
          <>
            <p className={cx(styles.status, !s.runtimeReady && styles.statusWarn)}>
              {s.runtimeStatus}
            </p>
            {s.runtimeSetup && (
              /* The fix, not the problem. A setup step the user can copy beats
                 a sentence telling them something is missing. */
              <pre className={styles.setup}>{s.runtimeSetup}</pre>
            )}
            {s.modelsDirProblem && (
              <p className={styles.dirProblem}>
                <Icon name="warning" size={11} />
                {s.modelsDirProblem}
              </p>
            )}

            <section className={styles.machine}>
              <h2 className={styles.machineName}>{s.machine}</h2>
              <p className={styles.machineTier}>{s.tierHeadline}</p>
              <dl className={styles.live}>
                <div className={styles.liveStat}>
                  <dt>free now</dt>
                  <dd className={s.sampleStale ? styles.stale : undefined}>
                    {s.sampleStale ? "not reporting" : bytes(s.availableBytes)}
                  </dd>
                </div>
                <div className={styles.liveStat}>
                  <dt>total</dt>
                  <dd>{bytes(s.totalBytes)}</dd>
                </div>
                <div className={styles.liveStat}>
                  <dt>load</dt>
                  <dd>{s.sustainedLoad.toFixed(2)}</dd>
                </div>
                <div className={styles.liveStat}>
                  <dt>models loaded</dt>
                  <dd>{s.residentBytes === 0 ? "none" : bytes(s.residentBytes)}</dd>
                </div>
              </dl>
              {s.sampleStale && (
                <p className={styles.note}>
                  <Icon name="warning" size={11} />
                  The hardware sampler has stopped reporting, so these figures are
                  not current. Nothing will be admitted on a reading this old.
                </p>
              )}
            </section>

            <section className={styles.section}>
              <h2 className={styles.heading}>AI</h2>
              <p className={styles.lede}>
                Moves the model that writes answers. The router and the embedder
                are unaffected — inflating them would cost memory for no gain.
              </p>
              <div className={styles.profiles} role="radiogroup" aria-label="AI preference">
                {s.profiles.map((p) => (
                  <button
                    key={p.id}
                    type="button"
                    role="radio"
                    aria-checked={p.selected}
                    disabled={!p.available || busy}
                    className={cx(styles.profile, p.selected && styles.profileOn)}
                    onClick={() => void choose(p.id)}
                  >
                    <span className={styles.profileLabel}>
                      {p.label}
                      {p.id === "balanced" && (
                        <span className={styles.recommended}>recommended</span>
                      )}
                    </span>
                    <span className={styles.profileDetail}>{p.detail}</span>
                    {p.unavailableReason && (
                      <span className={styles.profileWhy}>{p.unavailableReason}</span>
                    )}
                  </button>
                ))}
              </div>
              <div className={styles.roles}>
                <Role title="Router" r={s.router} />
                <Role title="Answers" r={s.generator} />
                <Role title="Search" r={s.embedder} />
              </div>
            </section>

            <section className={styles.section}>
              <div className={styles.headingRow}>
                <h2 className={styles.heading}>Detected</h2>
                <button
                  type="button"
                  className={styles.rescan}
                  onClick={() => void rescan()}
                  disabled={busy}
                >
                  Check again
                </button>
              </div>
              {s.detected.length === 0 ? (
                <p className={styles.lede}>
                  No local model server is running. Marrow looks for Ollama on
                  port 11434 and LM Studio on 1234 — if you start one, its models
                  appear here and nothing has to be downloaded.
                </p>
              ) : (
                <ul className={styles.detected}>
                  {s.detected.map((d) => (
                    <li key={d.runtime} className={styles.detectedItem}>
                      <Icon name="activity" size={14} />
                      <span className={styles.detectedName}>{d.runtime}</span>
                      <span className={styles.detectedPort}>port {d.port}</span>
                      <span className={styles.grow} />
                      <span className={styles.detectedCount}>
                        {d.modelCount} {d.modelCount === 1 ? "model" : "models"}
                      </span>
                    </li>
                  ))}
                </ul>
              )}
              {s.detectionProblems.map((p) => (
                <p key={p} className={styles.note}>
                  <Icon name="warning" size={11} />
                  {p}
                </p>
              ))}
            </section>

            <Semantic
              status={s.semantic}
              busy={busy}
              onStart={() => void run(startSemanticBackfill)}
              onStop={() => void run(stopSemanticBackfill)}
            />

            <section className={styles.section}>
              <h2 className={styles.heading}>Models</h2>
              <Answering
                snapshot={s}
                busy={busy}
                onPin={(id) => void act(() => setGeneratorModel(id), "")}
              />
              <ul className={styles.cards}>
                {s.models.map((m) => (
                  <ModelCard
                    key={m.id}
                    m={m}
                    busy={busy}
                    onDownload={(id) => void act(downloadModel, id)}
                    onCancel={(id) => void act(cancelModelDownload, id)}
                    onDismiss={(id) => void act(dismissModelDownload, id)}
                  />
                ))}
              </ul>
            </section>
          </>
        )}
      </div>
    </section>
  );
}

/**
 * Which model answers, and the control that decides it.
 *
 * **There was no way to choose one.** The largest installed model that fitted
 * the memory free at that instant won, so the choice moved with whatever else
 * happened to be running, nothing named it until the answer's footer, and
 * downloading a second model gave the user two models and no way to say which.
 *
 * "Whatever fits" stays the default and is shown as a choice rather than as an
 * absence, because an automatic decision the user cannot see is the thing that
 * made this confusing in the first place. A configured remote endpoint wins
 * over any local pin, and says so — a page showing a pinned local model while
 * a cloud endpoint answers would be lying about where the question goes.
 */
function Answering({
  snapshot,
  busy,
  onPin,
}: {
  snapshot: ModelsSnapshot;
  busy: boolean;
  onPin: (id: string | null) => void;
}) {
  const usable = snapshot.models.filter((m) => m.installed && m.role !== "embedder");
  const remote = snapshot.remote.configured && snapshot.remote.enabled;

  return (
    <div className={styles.answering}>
      <label className={styles.answeringLabel} htmlFor="answering-with">
        Answering with
      </label>
      <select
        id="answering-with"
        className={styles.answeringSelect}
        value={snapshot.pinnedModelId ?? ""}
        disabled={busy || remote || usable.length === 0}
        onChange={(e) => onPin(e.target.value === "" ? null : e.target.value)}
      >
        <option value="">Whatever fits this machine</option>
        {usable.map((m) => (
          <option key={m.id} value={m.id}>
            {m.displayName}
          </option>
        ))}
      </select>
      <p className={styles.note}>
        {remote ? (
          <>
            <strong>{snapshot.activeModel}</strong> answers, because a remote endpoint is
            configured and switched on in Settings. Turn it off there to use a local model.
          </>
        ) : usable.length === 0 ? (
          "No local model is installed yet, so there is nothing to choose between."
        ) : snapshot.pinnedModelId ? (
          <>
            <strong>{snapshot.activeModel}</strong> answers every question, whatever else is
            running.
          </>
        ) : (
          <>
            <strong>{snapshot.activeModel ?? "None"}</strong> would answer right now. Left on
            automatic this can change with the memory that happens to be free, so pin one if
            you want the same model every time.
          </>
        )}
      </p>
    </div>
  );
}

/**
 * How long the rest of the backfill will take, in words, or `null`.
 *
 * Measured here rather than guessed on the backend, from the figures two polls
 * apart — the rate depends on the machine, what else it is doing, and how long
 * the chunks are, so a constant baked into the app would be wrong everywhere
 * except where it was measured. Before it has two samples it says nothing;
 * `MEASURED_RATE` only carries the not-yet-running case, and it is labelled an
 * estimate because that is what it is.
 */
function useEta(status: SemanticStatus): string | null {
  // ~6.4 chunks/s, measured over a 54,687-chunk index with EmbeddingGemma 300M
  // on an M4 Pro. Only used before the first two samples exist.
  const MEASURED_RATE = 6.4;
  const last = useRef<{ embedded: number; at: number } | null>(null);
  const [rate, setRate] = useState<number | null>(null);

  useEffect(() => {
    if (!status.running) {
      last.current = null;
      setRate(null);
      return;
    }
    const now = Date.now();
    const prev = last.current;
    last.current = { embedded: status.embedded, at: now };
    if (!prev || now === prev.at) return;
    const observed = ((status.embedded - prev.embedded) * 1000) / (now - prev.at);
    if (observed <= 0) return;
    // Smoothed, or the number jitters with every poll and reads as unreliable.
    setRate((r) => (r === null ? observed : r * 0.7 + observed * 0.3));
  }, [status.running, status.embedded]);

  if (status.remaining === 0) return null;
  const perSecond = rate ?? (status.running ? null : MEASURED_RATE);
  if (!perSecond) return null;

  const minutes = Math.ceil(status.remaining / perSecond / 60);
  const when =
    minutes < 2
      ? "under a minute"
      : minutes < 90
        ? `about ${minutes} minutes`
        : `about ${Math.round(minutes / 60)} hours`;
  return status.running ? `${when} left.` : `Roughly ${when} on a machine like this.`;
}

/**
 * Semantic search — built on demand, and honest about how much it covers.
 *
 * Deliberately a button rather than something that runs on its own. It loads a
 * model and works for minutes, and keyword search is the half that must work
 * with no model, no GPU and no network (hard rule 10) — so the meaning-based
 * half is something the user turns on, not something that happens to them.
 *
 * The coverage figure is the point. A half-built index is not broken, but a
 * user whose results are quietly worse than they will be in ten minutes, with
 * nothing saying why, has no way to tell the two apart.
 */
function Semantic({
  status,
  busy,
  onStart,
  onStop,
}: {
  status: SemanticStatus;
  busy: boolean;
  onStart: () => void;
  onStop: () => void;
}) {
  const total = status.embedded + status.remaining;
  const pct = total > 0 ? Math.round((status.embedded / total) * 100) : 0;
  const complete = total > 0 && status.remaining === 0;
  const eta = useEta(status);

  return (
    <section className={styles.section}>
      <div className={styles.headingRow}>
        <h2 className={styles.heading}>Semantic search</h2>
        <button
          type="button"
          className={styles.rescan}
          onClick={status.running ? onStop : onStart}
          disabled={busy || (complete && !status.running)}
        >
          {status.running ? "Stop" : complete ? "Built" : "Build"}
        </button>
      </div>

      <p className={styles.lede}>
        {complete
          ? "Every chunk has a vector. Searches match on meaning as well as words."
          : status.embedded === 0
            ? "Not built. Search matches words exactly — which always works, with no model and no network. Building this adds matching on meaning."
            : `Covers ${pct}% of what is indexed. The rest is still keyword-only.`}
        {!complete && eta && ` ${eta}`}
      </p>

      {total > 0 && (
        <div className={styles.progress}>
          <div className={styles.bar}>
            <div
              className={cx(
                styles.barFill,
                complete && styles.barDone,
                !status.running && !complete && styles.barStopped,
              )}
              style={{ width: `${pct}%` }}
            />
          </div>
          <p className={styles.progressLine}>
            <span className={styles.progressStage}>
              {status.embedded.toLocaleString()} of {total.toLocaleString()} chunks
            </span>
            {status.model && <span>{status.model}</span>}
            {status.failed > 0 && (
              <span className={styles.progressFailed}>
                {status.failed.toLocaleString()} skipped
              </span>
            )}
          </p>
        </div>
      )}

      {/* Never silent: "off" and "cannot run" look identical otherwise. */}
      {status.problem && (
        <p className={styles.blocked}>
          <Icon name="warning" />
          {status.problem}
        </p>
      )}
    </section>
  );
}

/**
 * SKEL-001: a skeleton in the shape of the result, not a spinner. A spinner
 * says "wait"; a skeleton says "here is what is coming".
 */
function SkeletonPage() {
  return (
    <div className={styles.skeleton} aria-busy="true" aria-live="polite">
      <span className={styles.srOnly}>Reading this machine…</span>
      <div className={cx(styles.skel, styles.skelHead)} />
      <div className={cx(styles.skel, styles.skelLine)} />
      <div className={styles.skelRow}>
        {[0, 1, 2, 3].map((i) => (
          <div key={i} className={cx(styles.skel, styles.skelStat)} />
        ))}
      </div>
      {[0, 1, 2, 3].map((i) => (
        <div key={i} className={cx(styles.skel, styles.skelCard)} />
      ))}
    </div>
  );
}
