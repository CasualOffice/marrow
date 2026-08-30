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

import { useCallback, useState } from "react";

import styles from "./ModelsView.module.css";
import { cx } from "../lib/cx";
import { bytes } from "../lib/format";
import { StateBadge, type StateTone } from "./Badges";
import { ErrorNotice } from "./ErrorNotice";
import { Icon } from "./Icon";
import { useModels } from "../queries";
import {
  asUiError,
  refreshModelDetection,
  setAiProfile,
  type ModelRow,
  type ModelState,
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

function ModelCard({ m }: { m: ModelRow }) {
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

      <p className={styles.fitReason}>{m.fitReason}</p>
      <p className={styles.breakdown}>{m.breakdown}</p>

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

      <div className={styles.actions}>
        {m.installed ? (
          // The badge already says "installed", so repeating it here would be
          // two words for one fact. Show the lifecycle state only when it is
          // more than that, and the context window otherwise.
          <span className={styles.stateText}>
            {m.state.state === "installed" ? contextWindow(m) : stateWord(m.state)}
          </span>
        ) : m.downloadable ? (
          <button type="button" className={styles.get}>
            Download {bytes(m.requiredBytes)}
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

        {!s ? (
          <SkeletonPage />
        ) : (
          <>
            <p className={styles.status}>{s.runtimeStatus}</p>

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

            <section className={styles.section}>
              <h2 className={styles.heading}>Models</h2>
              <ul className={styles.cards}>
                {s.models.map((m) => (
                  <ModelCard key={m.id} m={m} />
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
