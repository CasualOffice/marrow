/**
 * Ask — a conversation about your files.
 *
 * A thread rather than a single box, because the second question is almost
 * always about the first: "and the rent?", "which file was that in?". Turns
 * are sent back with each request, which is also what makes the KV prefix
 * cache pay — the whole preamble is identical, so a follow-up reuses about
 * 80% of the prompt instead of re-reading every document.
 *
 * The order within a turn is the order of the pipeline: sources land before
 * the first token. An answer whose citations appear afterwards reads as though
 * they were found to fit.
 */

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { useQueryClient } from "@tanstack/react-query";

import styles from "./AskView.module.css";
import { cx } from "../lib/cx";
import { bytes, duration } from "../lib/format";
import { Answer } from "./Answer";
import { Icon } from "./Icon";
import { Kbd } from "./Kbd";
import {
  ask,
  asUiError,
  cancelAsk,
  forgetConversation,
  loadConversation,
  saveTurn,
  type AskEvent,
  type Citation,
  type ExcludedSource,
  type PriorTurn,
  type StoredTurn,
  type TurnUsage,
} from "../api";
import { CONVERSATIONS_KEY, useProjects } from "../queries";
import { useUi } from "../store";

type Usage = TurnUsage;

interface Turn {
  readonly id: string;
  readonly question: string;
  readonly thorough: boolean;
  answer: string;
  thinking: string;
  sources: readonly Citation[];
  excluded: readonly ExcludedSource[];
  /**
   * The projects the sources came from. More than one means this answer was
   * assembled across unrelated bodies of work — the workspace is one folder
   * and it holds a dozen services — and saying nothing about that presents
   * three projects as one coherent account of a single thing.
   */
  projects: readonly string[];
  meta: { boundary: string; model: string; bytes: number } | null;
  usage: Usage | null;
  failure: { code: string; message: string } | null;
  /** What the pipeline is doing, while it is doing it. */
  stage: { stage: string; detail: string } | null;
  /** True while tokens are still arriving. */
  running: boolean;
}

function newTurn(question: string, thorough: boolean): Turn {
  return {
    id: `${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    question,
    thorough,
    answer: "",
    thinking: "",
    sources: [],
    excluded: [],
    projects: [],
    meta: null,
    usage: null,
    failure: null,
    stage: { stage: "retrieving", detail: "Searching your files" },
    running: true,
  };
}

/**
 * The key the model runtime caches a conversation's KV prefix under.
 *
 * Deliberately **not** the stored conversation's identifier, and the two live
 * side by side for the whole life of a thread. A conversation gets its ULID from
 * the store when its first answer is saved, which is several seconds after the
 * question was asked; keying the session on that would mean the first turn had
 * no key, or that the key changed under the cache between turn one and turn two
 * and threw away the ~80% prefix reuse that is the entire reason the session
 * exists. Reopening a stored conversation *does* use its id as the key — by
 * then it is stable, and one identity is better than two.
 */
function sessionKey() {
  return `c-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

/** A stored turn, back in the shape the thread renders. */
function restore(t: StoredTurn, i: number): Turn {
  return {
    id: `stored-${i}`,
    question: t.question,
    thorough: t.thorough,
    answer: t.answer,
    // Reasoning is not persisted: GEN-015 says it is never evidence and never
    // cited, and it is the largest thing a turn produces. Its absence in a
    // reopened thread is the honest rendering of that.
    thinking: "",
    sources: t.citations,
    excluded: t.excluded,
    projects: t.projects,
    // `bytes: 0` reads as "not recorded" in the summary line, which is what it
    // is. Local is the only boundary this pipeline has ever had.
    meta: t.model === null ? null : { boundary: "local", model: t.model, bytes: 0 },
    usage: t.usage,
    failure: null,
    stage: null,
    running: false,
  };
}

export function AskView() {
  const [session, setSession] = useState(sessionKey);
  const [turns, setTurns] = useState<Turn[]>([]);
  const [question, setQuestion] = useState("");
  // In the store rather than in component state, because it has to survive the
  // process: a mode switch that resets on every launch is indistinguishable
  // from one that does not work, and the user re-chose it every morning.
  const thorough = useUi((s) => s.thorough);
  const setThorough = useUi((s) => s.setThorough);
  // `null` is every project, which is the right default: narrowing is something
  // you choose when you know you mean one, not a setting to get wrong first.
  const [scope, setScope] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const askId = useRef<string | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const stickToBottom = useRef(true);
  /**
   * The write for the previous turn, while it is still in flight.
   *
   * The first turn of a thread is what creates the conversation, and its
   * identifier only exists once that write comes back. A follow-up sent in the
   * gap would read "no conversation yet" and start a second one — two rows in
   * the list for one exchange, and neither of them the whole of it. Waiting is
   * a few milliseconds against a question that has just taken seconds.
   */
  const pendingSave = useRef<Promise<unknown> | null>(null);
  const setView = useUi((s) => s.setView);
  const notify = useUi((s) => s.notify);
  const epoch = useUi((s) => s.conversationEpoch);
  const client = useQueryClient();

  /* Read inside async closures that outlive the render that made them. */
  const sessionRef = useRef(session);
  sessionRef.current = session;
  const epochRef = useRef(epoch);
  epochRef.current = epoch;

  /**
   * Follow the stream, but stop the moment the user scrolls up. Yanking
   * someone back to the bottom while they are reading is the single most
   * irritating thing a streaming view can do.
   */
  useLayoutEffect(() => {
    const el = scroller.current;
    if (el && stickToBottom.current) el.scrollTop = el.scrollHeight;
  });

  const onScroll = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    stickToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
  }, []);

  useEffect(() => {
    field.current?.focus();
  }, []);

  const update = useCallback((id: string, f: (t: Turn) => Turn) => {
    setTurns((all) => all.map((t) => (t.id === id ? f(t) : t)));
  }, []);

  /** An empty thread with a session key nothing else has used. */
  const startFresh = useCallback(() => {
    // The runtime is holding a KV prefix for the thread being left. Nobody is
    // coming back to it under this key — reopening the conversation uses its
    // stored id — so releasing it now is a few hundred megabytes recovered
    // rather than held for a cache hit that cannot happen.
    void forgetConversation(sessionRef.current);
    setSession(sessionKey());
    setTurns([]);
    setQuestion("");
    setScope(null);
    field.current?.focus();
  }, []);

  /**
   * Show a stored conversation.
   *
   * Guarded on the epoch across the await: two clicks in quick succession must
   * not race, and the second one is the one the user meant.
   */
  const open = useCallback(
    async (id: string, atEpoch: number) => {
      try {
        const c = await loadConversation(id);
        if (epochRef.current !== atEpoch) return;
        void forgetConversation(sessionRef.current);
        setSession(id);
        setTurns(c.turns.map(restore));
        setScope(c.scope);
        setQuestion("");
        stickToBottom.current = true;
        field.current?.focus();
      } catch (e) {
        if (epochRef.current === atEpoch) notify(asUiError(e).message);
      }
    },
    [notify],
  );

  /*
   * The sidebar asks for a thread; this is where it arrives.
   *
   * Keyed on the epoch rather than on the id, so recording the id of a
   * conversation this view has just created does not reload the thread that is
   * already on screen. Anything still generating is stopped first: what it has
   * written by then is saved against its own conversation, not the one being
   * opened, because the target was captured when the question was sent.
   */
  const openedAt = useRef(epoch);
  useEffect(() => {
    if (openedAt.current === epoch) return;
    openedAt.current = epoch;
    if (askId.current) {
      void cancelAsk(askId.current);
      notify("Stopped the answer in progress. What was written is kept.");
    }
    const id = useUi.getState().activeConversationId;
    if (id === null) startFresh();
    else void open(id, epoch);
  }, [epoch, notify, open, startFresh]);

  /**
   * Write a finished exchange to the store.
   *
   * `into` is captured when the question is sent, not read here: a turn that
   * finishes after the user has moved on belongs to the conversation it was
   * asked in. For the same reason the returned id is only adopted when the
   * window is still on the thread that created it.
   */
  const persist = useCallback(
    async (
      into: string | null,
      turn: Turn,
      askedWithin: string | null,
      atEpoch: number,
    ) => {
      try {
        const saved = await saveTurn(into, {
          question: turn.question,
          answer: turn.answer,
          thorough: turn.thorough,
          model: turn.meta?.model ?? null,
          // The scope the question was asked under, not whatever the picker
          // says by the time the answer lands.
          scope: askedWithin,
          citations: turn.sources,
          excluded: turn.excluded,
          usage: turn.usage,
        });
        if (into === null && epochRef.current === atEpoch) {
          useUi.getState().setActiveConversationId(saved.id);
        }
        await client.invalidateQueries({ queryKey: CONVERSATIONS_KEY });
      } catch (e) {
        // Not fatal to the answer, which is on screen and readable. Said out
        // loud all the same: silence here means the thread is gone at quit and
        // nothing warned anybody.
        notify(`The answer could not be saved. ${asUiError(e).message}`);
      }
    },
    [client, notify],
  );

  const send = useCallback(
    async (text: string, mode: boolean) => {
      const q = text.trim();
      if (!q || running) return;
      // See `pendingSave`: this is what stops a fast follow-up starting a
      // second conversation for the same thread.
      if (pendingSave.current) await pendingSave.current;

      // Sent before this turn is appended, so the model sees the conversation
      // as it was when the question was asked.
      const history: PriorTurn[] = turns.flatMap((t) =>
        t.answer
          ? [
              { role: "user" as const, text: t.question },
              { role: "assistant" as const, text: t.answer },
            ]
          : [],
      );

      const turn = newTurn(q, mode);
      setTurns((all) => [...all, turn]);
      setQuestion("");
      setRunning(true);
      stickToBottom.current = true;

      /*
       * The conversation this answer belongs to, and the thread that is on
       * screen, decided **now**. Both can change while the model is talking —
       * the user can open another conversation, and the first turn of this one
       * creates a row that did not exist when it was asked.
       */
      const into = useUi.getState().activeConversationId;
      const atEpoch = epochRef.current;

      /*
       * A local copy of the turn, moved by the same functions that move the
       * rendered one. What gets written to the store is then the thing that was
       * displayed, by construction — reading it back out of component state
       * after the stream ends would be a second version of the same turn, and
       * the one that goes to disk is the one nobody looks at.
       */
      let record = turn;
      const applyToTurn = (f: (t: Turn) => Turn) => {
        record = f(record);
        update(turn.id, f);
      };

      const onEvent = (e: AskEvent) => {
        switch (e.kind) {
          case "started":
            // Before anything else, so Stop and Esc have something to cancel
            // for the whole time they are on screen.
            askId.current = e.id;
            break;
          case "stage":
            applyToTurn((t) => ({ ...t, stage: { stage: e.stage, detail: e.detail } }));
            break;
          case "sources":
            applyToTurn((t) => ({
              ...t,
              sources: e.hits,
              excluded: e.excluded,
              projects: e.projects ?? [],
              meta: { boundary: e.boundary, model: e.model, bytes: e.bytes },
            }));
            break;
          case "token":
            // The first token ends the waiting state: once text is arriving,
            // a stage line beside it would be describing the past.
            applyToTurn((t) => ({ ...t, answer: t.answer + e.text, stage: null }));
            break;
          case "thinking":
            // Reasoning arriving is output arriving. Leaving the skeleton up
            // through a long Thorough think would say "nothing yet" while the
            // model is visibly working.
            applyToTurn((t) => ({ ...t, thinking: t.thinking + e.text, stage: null }));
            break;
          case "done":
            applyToTurn((t) => ({ ...t, usage: e, stage: null }));
            break;
          case "failed":
            applyToTurn((t) => ({
              ...t,
              failure: { code: e.code, message: e.message },
            }));
            break;
        }
      };

      try {
        await ask(
          { conversation: session, question: q, history, thorough: mode, scope },
          onEvent,
        );
      } catch (e) {
        applyToTurn((t) => ({
          ...t,
          failure: {
            code: "UI_UNEXPECTED",
            message: e instanceof Error ? e.message : String(e),
          },
        }));
      } finally {
        applyToTurn((t) => ({ ...t, running: false, stage: null }));
        setRunning(false);
        askId.current = null;
        field.current?.focus();
      }

      // A cancelled answer is still an answer — it is what was written, and it
      // is what the reader will expect to find when they come back. A turn with
      // no text at all is a failure, and a list of questions that were never
      // answered is not a conversation worth keeping.
      if (record.answer.trim() !== "") {
        const write = persist(into, record, scope, atEpoch).finally(() => {
          if (pendingSave.current === write) pendingSave.current = null;
        });
        pendingSave.current = write;
      }
    },
    [persist, running, scope, session, turns, update],
  );

  const stop = useCallback(() => {
    if (askId.current) void cancelAsk(askId.current);
  }, []);

  const retry = useCallback(
    (t: Turn) => {
      // Asked again, at the end. It used to drop this turn and everything after
      // it first — which was fine while a thread lived only in this component
      // and is not now: the store has no notion of un-asking, so the view would
      // have shown one conversation today and a longer one tomorrow.
      void send(t.question, t.thorough);
    },
    [send],
  );

  /*
   * **Where the composer sits is the whole of the empty state.**
   *
   * Pinned to the bottom from the start, an unused Ask view is three lines of
   * grey text floating in a large void with the one control you want at the far
   * edge of it. Every chat product opens with the input in the middle under a
   * short greeting and drops it to the bottom once there is an answer to read,
   * for the same reason: before the first question the composer *is* the
   * content.
   *
   * One class, not a second layout: the scroller stops claiming the free height
   * and the column centres what is left, which is the greeting and the composer
   * as a pair. Nothing moves except by the flex rules already here, so there is
   * no animation to suppress under `prefers-reduced-motion`.
   */
  const opening = turns.length === 0;

  return (
    <section className={cx(styles.view, opening && styles.viewOpening)} aria-label="Ask">
      <div className={styles.scroll} ref={scroller} onScroll={onScroll}>
        {opening ? (
          <Empty />
        ) : (
          <ol className={styles.thread}>
            {turns.map((t) => (
              <TurnBlock key={t.id} turn={t} onRetry={() => retry(t)} onModels={() => setView("models")} />
            ))}
          </ol>
        )}
      </div>

      <form
        className={styles.composer}
        onSubmit={(e) => {
          e.preventDefault();
          void send(question, thorough);
        }}
      >
        <textarea
          ref={field}
          className={styles.field}
          value={question}
          placeholder={turns.length ? "Ask a follow-up…" : "Ask about your files…"}
          rows={1}
          onChange={(e) => setQuestion(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              void send(question, thorough);
            }
            if (e.key === "Escape" && running) {
              e.preventDefault();
              // The window binds Escape too, and there it clears the search
              // query and takes focus to the search field — so stopping an
              // answer would also throw you out of the composer you are still
              // typing in. It means one thing here.
              e.stopPropagation();
              stop();
            }
          }}
        />
        <div className={styles.controls}>
          {/* GEN-012: the switch is visible before the request is sent, next
              to what it costs, so the trade is legible while choosing. */}
          <ScopePicker value={scope} onChange={setScope} disabled={running} />
          <ModeSwitch value={thorough} onChange={setThorough} disabled={running} />
          <span className={styles.grow} />
          {running ? (
            <button type="button" className={styles.stop} onClick={stop}>
              Stop <Kbd>Esc</Kbd>
            </button>
          ) : (
            <button type="submit" className={styles.send} disabled={!question.trim()}>
              Ask <Kbd>↵</Kbd>
            </button>
          )}
        </div>
      </form>
    </section>
  );
}

/**
 * What the pipeline is doing, while it is doing it.
 *
 * SKEL-003: a skeleton that is still there at ten seconds becomes a status
 * with a reason. The first question of a session loads several gigabytes, so
 * that is not an edge case — it is the first thing anyone sees. The elapsed
 * counter is what makes a long load read as working rather than stuck.
 */
/**
 * Which project the question is about.
 *
 * **The gap this closes is the one that was reported.** One granted folder can
 * hold many unrelated projects — this workspace is `~/Desktop/melp`, which
 * contains a speech service, a vault, a task API and more — and retrieval had
 * no notion that they were different things. Asking "what is STT?" answered
 * from all of them at once, mixing in MFA settings and a Code of Conduct,
 * because every one of those documents is genuinely in the workspace and
 * genuinely contains the words.
 *
 * The projects come from the same derivation the answer uses when it names
 * which ones it drew from, so narrowing to a project you were just shown
 * narrows to that project.
 *
 * A native `select`, deliberately: it is one control, it is keyboard-navigable
 * and screen-readable without any code of ours, and a custom menu here would be
 * more chrome carrying no more meaning.
 */
function ScopePicker({
  value,
  onChange,
  disabled,
}: {
  value: string | null;
  onChange: (v: string | null) => void;
  disabled: boolean;
}) {
  const projects = useProjects();
  const rows = projects.data ?? [];
  // Below two there is nothing to choose between, and a control offering a
  // single option is furniture.
  if (rows.length < 2) return null;

  return (
    <label className={styles.scope}>
      <span className={styles.srOnly}>Limit the question to one project</span>
      <select
        className={styles.scopeSelect}
        value={value ?? ""}
        disabled={disabled}
        onChange={(e) => onChange(e.target.value === "" ? null : e.target.value)}
      >
        <option value="">All projects</option>
        {rows.map((p) => (
          <option key={p.path} value={p.path}>
            {p.path} · {p.files.toLocaleString()}
          </option>
        ))}
      </select>
    </label>
  );
}

/**
 * Fast or Thorough, as one control instead of two cards.
 *
 * It was two ~280×56px buttons on a row of their own: 560px of the composer
 * spent on a binary choice, sitting above the input that is the reason anyone
 * is on this screen at all. GEN-012 wants the trade legible before the request
 * is sent, and one short line describing the mode you are actually in does that
 * — the alternative's second caption describes an option you did not pick.
 */
function ModeSwitch({
  value,
  onChange,
  disabled,
}: {
  value: boolean;
  onChange: (v: boolean) => void;
  disabled: boolean;
}) {
  return (
    <div className={styles.mode}>
      <div className={styles.modes} role="radiogroup" aria-label="Answer mode">
        {([false, true] as const).map((t) => (
          <button
            key={String(t)}
            type="button"
            role="radio"
            aria-checked={value === t}
            className={cx(styles.modeBtn, value === t && styles.modeOn)}
            title={
              t
                ? "Reasons before answering. Slower, and better on a question that needs several sources put together."
                : "Answers straight from the evidence."
            }
            onClick={() => onChange(t)}
            disabled={disabled}
          >
            {t ? "Thorough" : "Fast"}
          </button>
        ))}
      </div>
      <span className={styles.modeWhy}>
        {value ? "reasons first, slower" : "straight answer"}
      </span>
    </div>
  );
}

function Waiting({ stage }: { stage: { stage: string; detail: string } }) {
  const [seconds, setSeconds] = useState(0);
  useEffect(() => {
    setSeconds(0);
    const t = window.setInterval(() => setSeconds((s) => s + 1), 1000);
    return () => window.clearInterval(t);
  }, [stage.stage]);

  return (
    <div className={styles.waiting} aria-busy="true" aria-live="polite">
      <div className={styles.waitingLine}>
        <span className={styles.waitingDot} aria-hidden="true" />
        <span className={styles.waitingText}>{stage.detail}</span>
        {/* Only once it has been long enough to wonder. A counter that starts
            at 1 s makes every fast answer look slow. */}
        {seconds >= 3 && <span className={styles.waitingClock}>{seconds}s</span>}
      </div>
      {/* SKEL-001: a skeleton in the shape of the result, not a spinner. */}
      <div className={styles.skeleton}>
        <span className={styles.skelLine} style={{ width: "92%" }} />
        <span className={styles.skelLine} style={{ width: "78%" }} />
        <span className={styles.skelLine} style={{ width: "85%" }} />
      </div>
      {seconds >= 20 && stage.stage === "loading" && (
        <p className={styles.waitingNote}>
          Still loading. A model is a few gigabytes and this happens once per
          session; the next question will be quick.
        </p>
      )}
    </div>
  );
}

function Empty() {
  return (
    <div className={styles.empty}>
      <h2 className={styles.emptyHead}>Ask about your files</h2>
      <p className={styles.emptyBody}>
        Answers come from what is indexed on this machine, with a citation for
        every claim. Nothing is sent anywhere.
      </p>
    </div>
  );
}

function TurnBlock({
  turn,
  onRetry,
  onModels,
}: {
  turn: Turn;
  onRetry: () => void;
  onModels: () => void;
}) {
  const [copied, setCopied] = useState(false);
  const citationIds = new Set(turn.sources.map((s) => s.id));

  return (
    <li className={styles.turn}>
      <p className={styles.question}>{turn.question}</p>

      {turn.failure && (
        <div className={styles.failure}>
          <Icon name="warning" size={14} />
          <div>
            <p className={styles.failureText}>{turn.failure.message}</p>
            {turn.failure.code === "MOD_NOT_INSTALLED" && (
              <button type="button" className={styles.linkBtn} onClick={onModels}>
                Open Models
              </button>
            )}
          </div>
        </div>
      )}

      {turn.sources.length > 0 && (
        <details className={styles.sources}>
          <summary>
            {turn.sources.length} {turn.sources.length === 1 ? "source" : "sources"}
            {/* Surfaced in the summary, not only inside it: "why wasn't my
                file used" must be answerable without opening anything. */}
            {turn.excluded.length > 0 && (
              <span className={styles.sourcesExcluded}>
                {turn.excluded.length} not used
              </span>
            )}
            {/* `0` is a reopened turn, where the size of the prompt was not
                recorded. "0 B of context" would be a measurement; saying
                nothing is the absence it actually is. */}
            {turn.meta && turn.meta.bytes > 0 && (
              <span className={styles.sourcesBytes}>{bytes(turn.meta.bytes)} of context</span>
            )}
            {/* Said here rather than only inside the list, because it changes
                how the answer above should be read: evidence drawn from three
                unrelated projects is not one account of one thing, and the
                reader is the only one who can tell whether that was wanted. */}
            {turn.projects.length > 1 && (
              <span className={styles.sourcesProjects}>
                across {turn.projects.length} projects: {turn.projects.join(", ")}
              </span>
            )}
          </summary>
          <ol className={styles.sourceList}>
            {turn.sources.map((s) => (
              <li key={s.id} id={`cite-${turn.id}-${s.id}`} className={styles.source}>
                <span className={styles.sourceId}>{s.id}</span>
                <div className={styles.sourceBody}>
                  <span className={styles.sourcePath}>{s.location}</span>
                  <span className={styles.sourceExcerpt}>{s.excerpt}</span>
                </div>
              </li>
            ))}
          </ol>
          {turn.excluded.length > 0 && (
            /* Silence would look like these were never found. */
            <ul className={styles.excluded}>
              {turn.excluded.map((x) => (
                <li key={x.relativePath}>
                  {x.relativePath} — {x.reason}
                </li>
              ))}
            </ul>
          )}
        </details>
      )}

      {turn.thinking && (
        <details className={styles.thinking}>
          <summary>
            Reasoning
            <span className={styles.thinkingNote}>
              the model's working — not evidence, and never cited
            </span>
          </summary>
          <pre className={styles.thinkingText}>{turn.thinking}</pre>
        </details>
      )}

      {turn.stage && <Waiting stage={turn.stage} />}

      {/* Not while waiting: the skeleton is already saying "something is
          coming", and a caret beside it is two answers to one question. */}
      {(turn.answer || (turn.running && !turn.stage)) && (
        <Answer
          text={turn.answer}
          citations={citationIds}
          streaming={turn.running}
          onCite={(id) => {
            const el = document.getElementById(`cite-${turn.id}-${id}`);
            // The list is collapsed by default, so open it before scrolling —
            // otherwise the click appears to do nothing.
            el?.closest("details")?.setAttribute("open", "");
            el?.scrollIntoView({ block: "center", behavior: "smooth" });
          }}
        />
      )}

      {/* Where the answer stops, not only in the footer. An answer that ends
          mid-sentence with the reason in small grey type below reads as broken
          rather than as truncated. */}
      {turn.usage?.stopReason === "length" && (
        <p className={styles.cutOff}>
          <Icon name="warning" size={11} />
          This answer reached its length limit and stopped here. Ask a narrower
          question, or ask it to continue.
        </p>
      )}
      {turn.usage?.stopReason === "cancelled" && (
        <p className={styles.cutOff}>
          <Icon name="warning" size={11} />
          Stopped. What is above is what had been written.
        </p>
      )}

      {turn.usage && (
        <div className={styles.footer}>
          <span className={styles.usage}>
            {turn.meta && `${turn.meta.model} · ${turn.meta.boundary === "local" ? "on this device" : turn.meta.boundary} · `}
            {turn.usage.outputTokens} tokens in {duration(turn.usage.elapsedMs)}
            {turn.usage.thinkingTokens > 0 && ` · ${turn.usage.thinkingTokens} thinking`}
            {turn.usage.cachedPrefixTokens > 0 &&
              ` · ${Math.round(
                (turn.usage.cachedPrefixTokens / Math.max(1, turn.usage.promptTokens)) * 100,
              )}% of the prompt reused`}
            {turn.usage.stopReason === "length" && (
              <span className={styles.truncated}> · cut off at the token limit</span>
            )}
            {turn.usage.stopReason === "cancelled" && (
              <span className={styles.truncated}> · stopped</span>
            )}
          </span>
          <span className={styles.grow} />
          <button
            type="button"
            className={styles.ghost}
            onClick={() => {
              void navigator.clipboard?.writeText(turn.answer);
              setCopied(true);
              window.setTimeout(() => setCopied(false), 1400);
            }}
          >
            {copied ? "Copied" : "Copy"}
          </button>
          <button type="button" className={styles.ghost} onClick={onRetry}>
            Retry
          </button>
        </div>
      )}
    </li>
  );
}
