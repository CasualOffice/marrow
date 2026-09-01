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

import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
} from "react";
import { useQueryClient } from "@tanstack/react-query";

import styles from "./AskView.module.css";
import { cx } from "../lib/cx";
import { bytes, duration } from "../lib/format";
import { Answer } from "./Answer";
import { ProvenanceBadge } from "./Badges";
import { openInSystem, revealInFileManager } from "../actions";
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
  type SourceSpan,
} from "../api";
import { CONVERSATIONS_KEY, useModels, useProjects } from "../queries";
import { useUi } from "../store";

type Usage = TurnUsage;

interface Turn {
  readonly id: string;
  readonly question: string;
  readonly thorough: boolean;
  answer: string;
  thinking: string;
  /**
   * What the provider said that was neither answer nor failure — a setting it
   * could not honour, a frame it could not read, a plan limit about to bite.
   *
   * Kept per turn rather than as a toast: it qualifies *this* answer, and a
   * notice that has scrolled away cannot do that. An event with no case here
   * renders nothing and fails silently, which is the bug already recorded on
   * `AskEvent` — so it has one.
   */
  notices: readonly { message: string; code: string | null }[];
  sources: readonly Citation[];
  excluded: readonly ExcludedSource[];
  /**
   * The projects the sources came from. More than one means this answer was
   * assembled across unrelated bodies of work — the workspace is one folder
   * and it holds a dozen services — and saying nothing about that presents
   * three projects as one coherent account of a single thing.
   */
  projects: readonly string[];
  /**
   * Where this answer is being produced, and what is going there.
   *
   * Present from the *sources* event onward — before the first token — so
   * UX-012 is satisfied during the generation rather than in the footer
   * afterwards. `boundaryLabel` comes from Rust: one set of words, owned by
   * the side that decides which one is true.
   */
  meta: {
    boundary: string;
    boundaryLabel: string;
    destination: string | null;
    model: string;
    bytes: number;
    excerpts: number;
    files: number;
  } | null;
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
    notices: [],
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
 * A question typed while an answer was still streaming.
 *
 * **Exactly one, replaced rather than appended to.** The composer used to be
 * dead for the whole of a generation, which on a Thorough answer is a long time
 * to sit on a thought — but a queue of several is a backlog the user cannot see
 * the shape of, and that is worse than being told no. One is visible in a chip,
 * one is cancellable, and typing a second replaces the first in front of them.
 */
interface Queued {
  readonly text: string;
  readonly thorough: boolean;
  /**
   * The thread it was queued in, so it cannot be fired into a different one.
   * The user can open another conversation while an answer is still arriving.
   */
  readonly atEpoch: number;
  /**
   * Set when the run it was waiting behind ended badly, or ended somewhere
   * else. It is then neither sent nor thrown away: it stays on screen and
   * waits to be sent by hand. See the effect that decides this.
   */
  readonly held: boolean;
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
    notices: [],
    sources: t.citations,
    excluded: t.excluded,
    projects: t.projects,
    // `bytes: 0` reads as "not recorded" in the summary line, which is what
    // it is. The boundary is read back from what was stored with the turn —
    // **not defaulted to "local"**, which is what it used to do on the
    // reasoning that local was the only boundary this pipeline had ever had.
    // That reasoning expired the day an endpoint could be configured, and a
    // turn saved before the field existed has no boundary rather than a
    // reassuring one.
    meta:
      t.model === null
        ? null
        : {
            boundary: t.usage?.boundary ?? "",
            boundaryLabel: t.usage?.boundaryLabel ?? "",
            destination: null,
            model: t.model,
            bytes: 0,
            excerpts: t.citations.length,
            files: 0,
          },
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
  /** The one question waiting behind the answer on screen, if any. */
  const [queued, setQueued] = useState<Queued | null>(null);
  /**
   * `running`, but readable synchronously.
   *
   * `setRunning(true)` happens after `send` awaits the previous turn's write,
   * so for those few milliseconds the state says idle while a question is
   * already on its way. Deciding "queue or send" from the state alone would
   * start two generations in that window — rare before the composer could be
   * used at all during a run, and reachable by a keypress now.
   */
  const inFlight = useRef(false);
  /**
   * Whether the last generation ended with an answer, rather than with a
   * failure or a Stop. Read by the queue, and nothing else.
   */
  const lastRunOk = useRef(false);
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
  // The app's one always-mounted live region, in `Notice`.
  const announce = useUi((s) => s.announce);
  const epoch = useUi((s) => s.conversationEpoch);
  const client = useQueryClient();

  /* Read inside async closures that outlive the render that made them. */
  const sessionRef = useRef(session);
  sessionRef.current = session;
  const epochRef = useRef(epoch);
  epochRef.current = epoch;
  // Read by the effect that swaps the thread. As a dependency it would make
  // that effect re-run — and re-open the conversation — every time someone
  // queued a question.
  const queuedRef = useRef(queued);
  queuedRef.current = queued;

  /**
   * Follow the stream, but stop the moment the user scrolls up. Yanking
   * someone back to the bottom while they are reading is the single most
   * irritating thing a streaming view can do.
   *
   * **Decided from the gesture, not from where the scroller ended up**, which
   * is what it used to do and why scrolling during a stream was impossible.
   * `scroll` events are dispatched asynchronously, and this layout effect has
   * no dependency array — it runs after every render, which during a stream is
   * many times a second, synchronously before paint. So the order was:
   *
   *   1. the user scrolls up; the browser moves `scrollTop` and *queues* a
   *      scroll event
   *   2. a token arrives, React re-renders, and this effect slams `scrollTop`
   *      back to the bottom before that event is ever delivered
   *   3. the handler finally runs, measures a scroller that is now at the
   *      bottom, and concludes the user wants to stick
   *
   * The user's scroll was undone and then read as consent to undo it. `wheel`
   * and `touchmove` fire *before* the scroll is applied and are unambiguously a
   * person, so they release the lock outright and nothing can re-take it until
   * the user comes back to the bottom themselves.
   */
  const selfScrolling = useRef(false);

  useLayoutEffect(() => {
    const el = scroller.current;
    if (!el || !stickToBottom.current) return;
    const bottom = el.scrollHeight - el.clientHeight;
    // Only when it actually has to move. Assigning the position it already
    // holds fires no event, and the guard below would then swallow the user's
    // next real scroll instead.
    if (Math.abs(el.scrollTop - bottom) < 1) return;
    selfScrolling.current = true;
    el.scrollTop = bottom;
  });

  /** The user took hold of the scroller. */
  const release = useCallback(() => {
    stickToBottom.current = false;
  }, []);

  /**
   * The keyboard scrolls too, and the first version of this fix forgot.
   *
   * `release` was bound to `wheel` and `touchmove` only, which covers a
   * trackpad and a finger and nothing else. Page Up, the arrows, Home, End and
   * space all move the scroller without emitting either, so the layout effect
   * won the same race it used to win and yanked the reader back on the next
   * token — for the input GUI §5.1 calls primary.
   *
   * Only fires for keys that actually scroll, and only from inside the
   * scroller: the composer is not a descendant, so typing an arrow key in it
   * never reaches here.
   */
  const onKeyDown = useCallback(
    (e: ReactKeyboardEvent<HTMLDivElement>) => {
      const scrolls =
        e.key === "PageUp" ||
        e.key === "PageDown" ||
        e.key === "Home" ||
        e.key === "End" ||
        e.key === "ArrowUp" ||
        e.key === "ArrowDown" ||
        e.key === " ";
      if (scrolls) release();
    },
    [release],
  );

  const onScroll = useCallback(() => {
    const el = scroller.current;
    if (!el) return;
    if (selfScrolling.current) {
      // Ours, not theirs. Following the stream must not read as a decision to
      // follow the stream.
      selfScrolling.current = false;
      return;
    }
    stickToBottom.current = el.scrollHeight - el.scrollTop - el.clientHeight < 48;
  }, []);

  useEffect(() => {
    field.current?.focus();
  }, []);

  const update = useCallback((id: string, f: (t: Turn) => Turn) => {
    setTurns((all) => all.map((t) => (t.id === id ? f(t) : t)));
  }, []);

  /**
   * An empty thread with a session key nothing else has used.
   *
   * `draft` is a question that was queued against the thread being left. It
   * lands in the composer of the new one rather than being sent or discarded —
   * see the epoch effect below.
   */
  const startFresh = useCallback((draft = "") => {
    // The runtime is holding a KV prefix for the thread being left. Nobody is
    // coming back to it under this key — reopening the conversation uses its
    // stored id — so releasing it now is a few hundred megabytes recovered
    // rather than held for a cache hit that cannot happen.
    void forgetConversation(sessionRef.current);
    setSession(sessionKey());
    setTurns([]);
    setQuestion(draft);
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
    async (id: string, atEpoch: number, draft = "") => {
      try {
        const c = await loadConversation(id);
        if (epochRef.current !== atEpoch) return;
        void forgetConversation(sessionRef.current);
        setSession(id);
        setTurns(c.turns.map(restore));
        setScope(c.scope);
        setQuestion(draft);
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
    /*
     * A question queued against the thread being left goes with the user, into
     * the composer of whatever is opened next.
     *
     * Not sent: it was asked of a conversation that is no longer on screen,
     * and the run it was waiting behind has just been cancelled two lines up.
     * Not dropped either — they typed it, and losing someone's words because
     * they clicked a row in the sidebar is the kind of small theft nobody
     * forgives. Both paths below clear the composer, so there is no draft here
     * for this to overwrite.
     */
    const carried = queuedRef.current?.text ?? "";
    setQueued(null);
    const id = useUi.getState().activeConversationId;
    if (id === null) startFresh(carried);
    else void open(id, epoch, carried);
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
          // The boundary rides in `usage`, which is the window's own opaque
          // JSON in the store. Stored rather than re-derived on reopening,
          // for the same reason the citations are: what a turn shows later
          // should be what it showed when it was answered.
          usage:
            turn.usage === null
              ? null
              : {
                  ...turn.usage,
                  boundary: turn.meta?.boundary ?? null,
                  boundaryLabel: turn.meta?.boundaryLabel ?? null,
                },
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
      if (!q || inFlight.current) return;
      // Claimed here rather than after the await below, so nothing can start a
      // second generation in the gap. Cleared in the `finally`.
      inFlight.current = true;
      // See `pendingSave`: this is what stops a fast follow-up starting a
      // second conversation for the same thread. It covers the queued
      // follow-up too, which arrives through this same function.
      //
      // `persist` reports its own failures and never rejects; the `catch` is
      // so that a promise which somehow did could not leave `inFlight` stuck
      // and the composer dead for the rest of the session.
      if (pendingSave.current) await pendingSave.current.catch(() => undefined);

      // Sent before this turn is appended, so the model sees the conversation
      // as it was when the question was asked.
      const history: PriorTurn[] = turns.flatMap((t) =>
        t.answer
          ? [
              { role: "user" as const, text: t.question, truncated: false },
              {
                role: "assistant" as const,
                text: t.answer,
                // Sent, not just drawn. Without it the model reads its own
                // half-finished answer as finished and "continue" writes a
                // fresh introduction instead of resuming.
                truncated: t.usage?.stopReason === "length",
              },
            ]
          : [],
      );

      const turn = newTurn(q, mode);
      setTurns((all) => [...all, turn]);
      setQuestion("");
      // Or it keeps the height of the question that has just gone.
      if (field.current) grow(field.current);
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
              meta: {
                boundary: e.boundary,
                boundaryLabel: e.boundaryLabel,
                destination: e.destination,
                model: e.model,
                bytes: e.bytes,
                excerpts: e.hits.length,
                files: e.distinctSources,
              },
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
          case "notice":
            // Appended, not replaced: two different things going wrong is two
            // things the reader should see.
            applyToTurn((t) => ({
              ...t,
              notices: [...t.notices, { message: e.message, code: e.code }],
            }));
            break;
          case "done":
            applyToTurn((t) => ({ ...t, usage: e, stage: null }));
            /*
             * **Said out loud, because the whole turn was silent.**
             *
             * Nothing here carried a live region: 430 characters of answer
             * arrived and a screen reader was told none of it, and a *failed*
             * answer said nothing at all — in the one place in this app where
             * waiting is measured in tens of seconds.
             *
             * A short state sentence, not the prose. Streaming every token
             * into a live region is worse than silence: it interrupts itself
             * on every frame and the listener hears a stutter rather than an
             * answer. This is the moment worth interrupting for, and the
             * number of sources is the part nobody can see coming.
             *
             * Through the store's own always-mounted region rather than a new
             * one, because a live region added at the moment it has something
             * to say is usually not announced at all — the browser has to have
             * been watching it beforehand.
             *
             * Outside the updater, reading `record`. `applyToTurn` runs its
             * function twice — once on the local copy, once inside React's
             * state updater — so a side effect in there fires twice and does
             * it during an update, which is the one place a setter must not
             * be called. `record` is maintained here for exactly this: reading
             * the settled turn without waiting for a render.
             */
            announce(
              record.sources.length === 0
                ? "Answer complete, with no sources."
                : `Answer complete, ${record.sources.length} ${
                    record.sources.length === 1 ? "source" : "sources"
                  }.`,
            );
            break;
          case "failed":
            applyToTurn((t) => ({
              ...t,
              failure: { code: e.code, message: e.message },
            }));
            // The silent case: a failure a sighted reader sees in red was not
            // announced at all.
            announce(`The answer failed. ${e.message}`);
            break;
        }
      };

      try {
        await ask(
          { conversation: session, question: q, history, thorough: mode, scope },
          onEvent,
        );
      } catch (e) {
        const message = e instanceof Error ? e.message : String(e);
        announce(`The answer failed. ${message}`);
        applyToTurn((t) => ({
          ...t,
          failure: { code: "UI_UNEXPECTED", message },
        }));
      } finally {
        applyToTurn((t) => ({ ...t, running: false, stage: null }));
        // What the queue is allowed to do next. "Ended with an answer" and not
        // merely "ended": a Stop or a failure leaves the thread in a state a
        // follow-up would be asked *about*.
        lastRunOk.current =
          record.failure === null &&
          record.usage?.stopReason !== "cancelled" &&
          record.answer.trim() !== "";
        inFlight.current = false;
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
    [persist, scope, session, turns, update],
  );

  /*
   * The queued question, once the answer it was waiting behind has finished.
   *
   * **Sent only after a clean finish, and only in the thread it was queued
   * in.** A follow-up asked of a run that failed or was stopped is asked with
   * a broken or half-written answer as its context, so one failure becomes
   * two and the second one looks like the model's fault; and if the epoch has
   * moved the user is reading a different conversation, where a question they
   * asked of another one would arrive from nowhere. In both cases it is
   * *held* rather than dropped — still on screen, still cancellable, and sent
   * only if they press Send. Silently discarding what someone typed is the
   * other way to get this wrong.
   *
   * `send` awaits `pendingSave` before it builds the history, and that promise
   * is assigned synchronously at the end of the run above — before React can
   * flush the `running` change that triggers this effect. So the auto-sent
   * follow-up is behind the same guard a typed one is, and cannot start a
   * second conversation for this thread.
   */
  useEffect(() => {
    // `inFlight` as well as `running`, and for the same reason `send` reads it:
    // between the two there is a window where the state says idle and a
    // question is already on its way. Firing here then would have `send` refuse
    // it, and a queued question that vanishes on its own is the worst of the
    // three outcomes. Nothing is missed by waiting — `running` changes when
    // `inFlight` does, so this effect runs again either way.
    if (running || inFlight.current || queued === null || queued.held) return;
    if (!lastRunOk.current || queued.atEpoch !== epoch) {
      setQueued({ ...queued, held: true });
      return;
    }
    setQueued(null);
    void send(queued.text, queued.thorough);
  }, [epoch, queued, running, send]);

  /**
   * Enter, and the button beside it.
   *
   * Queues instead of refusing while an answer is streaming: the composer used
   * to drop the keystroke on the floor, which reads as a broken input rather
   * than as a rule.
   */
  const submit = useCallback(() => {
    const q = question.trim();
    if (!q) return;
    if (running || inFlight.current) {
      setQueued({ text: q, thorough, atEpoch: epoch, held: false });
      setQuestion("");
      // Or it keeps the height of the question that has just gone.
      if (field.current) grow(field.current);
      return;
    }
    void send(q, thorough);
  }, [epoch, question, running, send, thorough]);

  /** The held question, sent because the user said so. */
  const sendQueued = useCallback(() => {
    const q = queued;
    if (!q) return;
    setQueued(null);
    void send(q.text, q.thorough);
  }, [queued, send]);

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
      {/* The conversation finder used to sit here, above the thread, taking the
          top of the reading column on every answer — a control for the *list*
          of conversations, rendered over the one you are reading. It is in the
          sidebar now, immediately above the list it searches, which is where
          every product with a thread list puts it. It was only ever here
          because the sidebar was out of scope for the change that added it. */}
      <div
        className={styles.scroll}
        ref={scroller}
        onScroll={onScroll}
        onWheel={release}
        onTouchMove={release}
        onKeyDown={onKeyDown}
      >
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
          submit();
        }}
      >
        {queued && (
          <QueuedNext
            queued={queued}
            onSend={sendQueued}
            onCancel={() => setQueued(null)}
          />
        )}
        <textarea
          ref={field}
          className={styles.field}
          value={question}
          placeholder={
            running
              ? "Ask the next one — it is sent when this answer finishes…"
              : turns.length
                ? "Ask a follow-up…"
                : "Ask about your files…"
          }
          rows={1}
          onChange={(e) => {
            setQuestion(e.target.value);
            grow(e.currentTarget);
          }}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              submit();
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
            <>
              {/* Only once there is something to queue. A permanent second
                  button beside Stop would advertise a mode nobody is in. */}
              {question.trim() !== "" && (
                <button type="submit" className={styles.queue}>
                  Queue <Kbd>↵</Kbd>
                </button>
              )}
              <button type="button" className={styles.stop} onClick={stop}>
                Stop <Kbd>Esc</Kbd>
              </button>
            </>
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
 * The one question waiting behind the answer on screen.
 *
 * **Visible, and cancellable.** A queue whose contents you cannot see is a
 * promise that something will happen later without saying what — and the first
 * time it fires a question the user had forgotten about, they stop trusting the
 * composer. It sits directly above the field it was typed in.
 */
function QueuedNext({
  queued,
  onSend,
  onCancel,
}: {
  queued: Queued;
  onSend: () => void;
  onCancel: () => void;
}) {
  return (
    <div
      className={cx(styles.queued, queued.held && styles.queuedHeld)}
      // Announced, because the user's eyes are on the answer above it.
      role="status"
      aria-live="polite"
    >
      <span className={styles.queuedLabel}>{queued.held ? "Not sent" : "Next"}</span>
      <span className={styles.queuedText}>{queued.text}</span>
      {queued.held && (
        <>
          <span className={styles.queuedWhy}>
            the answer before it did not finish
          </span>
          <button type="button" className={styles.queuedSend} onClick={onSend}>
            Send anyway
          </button>
        </>
      )}
      <button
        type="button"
        className={styles.queuedCancel}
        onClick={onCancel}
        aria-label="Cancel the queued question"
        title="Cancel the queued question"
      >
        <Icon name="close" size={11} />
      </button>
    </div>
  );
}

/**
 * Find a conversation by what was said in it.
 *
 * The sidebar lists threads by recency under a title derived from their *first*
 * question, which is a serviceable index of ten conversations and none at all
 * of two hundred: the thing you remember is almost never the thing the thread
 * opened with. Searching the turns as well is what makes a long list navigable,
 * and a match deep in a thread arrives with the words that matched it — a
 * result that answers "is this the one?" with a title the sidebar already
 * showed you has answered nothing.
 *
 * Empty is not a search: the panel then shows the recent list, so this doubles
 * as a jump-to-conversation for anyone who never types anything into it.
 */

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
      {/* An em dash, because without one this sat beside two buttons at the
          same baseline and read as a third, unselected option. It is a caption
          for whichever is chosen. */}
      <span className={styles.modeWhy}>
        — {value ? "reasons first, slower" : "straight answer"}
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
            at 1 s makes every fast answer look slow.

            `aria-hidden`, because it ticks: inside a live region a 45-second
            model load would announce forty-two times, and "12s… 13s… 14s" is
            the least useful thing a screen reader could be given while
            waiting. The stage text beside it is what carries the meaning. */}
        {seconds >= 3 && (
          <span className={styles.waitingClock} aria-hidden="true">
            {seconds}s
          </span>
        )}
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

/**
 * Where the answers actually go, rather than where they used to.
 *
 * This sentence read "Nothing is sent anywhere." — true when it was written,
 * and false the moment an endpoint is configured. The empty state is the first
 * thing a new user reads about this window's privacy posture, which makes it
 * the worst place in the app to be out of date, so it is now read off what is
 * configured rather than typed.
 */
function WhereAnswersGo() {
  const models = useModels();
  const remote = models.data?.remote;
  if (!remote?.enabled) {
    return <>Nothing is sent anywhere.</>;
  }
  return (
    <>
      Answers are generated by {remote.label}
      {remote.boundaryLabel ? ` — ${remote.boundaryLabel}` : ""}, so the
      question and the excerpts it uses leave this device. Settings has the
      switch.
    </>
  );
}

function Empty() {
  return (
    <div className={styles.empty}>
      <h2 className={styles.emptyHead}>Ask about your files</h2>
      <p className={styles.emptyBody}>
        Answers come from what is indexed on this machine, with a citation for
        every claim. {<WhereAnswersGo />}
      </p>
    </div>
  );
}

/**
 * Size the composer to what has been typed, up to the CSS `max-height`.
 *
 * It was `rows={1}` with nothing resizing it, so a three-line question was
 * typed into a one-line box that scrolled its own first line out of sight —
 * and the only remedy was the drag handle, which nothing advertises. Done here
 * rather than with `field-sizing: content`, which WebKit did not ship until
 * recently and this app targets the WebView it is given.
 *
 * Reset to `auto` first: without it `scrollHeight` is measured against the
 * height already set, so the field can grow and never shrink again.
 */
function grow(el: HTMLTextAreaElement): void {
  el.style.height = "auto";
  el.style.height = `${el.scrollHeight}px`;
}

/**
 * The part of a citation after the filename — `:42`, `:p17`, `:Q2!B4:B18`.
 *
 * Reads the span rather than `line`, which cannot express a page or a cell.
 * `null` where the format has no notation a person can act on: a byte offset
 * is not a location, and inventing one for the rest would claim a precision
 * nothing supports. Mirrors `SourceSpan::locate` in Rust — the two disagreeing
 * is exactly the bug this replaced, so keep them together.
 */
function where(c: { span: SourceSpan; line: number | null }): string | null {
  switch (c.span.kind) {
    case "lines":
      return `:${c.span.start}`;
    case "page":
      return `:p${c.span.page}`;
    case "cells":
      return `:${c.span.sheet}!${c.span.range}`;
    default:
      return c.line === null ? null : `:${c.line}`;
  }
}

/** Everything up to and including the last `/`, or empty for a bare filename. */
function dirOf(rel: string): string {
  const cut = rel.lastIndexOf("/");
  return cut === -1 ? "" : rel.slice(0, cut + 1);
}

/** The filename. Never truncated — see the note at its call site. */
function nameOf(rel: string): string {
  const cut = rel.lastIndexOf("/");
  return cut === -1 ? rel : rel.slice(cut + 1);
}

/** How many of these citations point at something less than an exact span. */
function inexact(sources: readonly { provenance: string }[]): number {
  return sources.filter((s) => s.provenance !== "exact").length;
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

      {/* UX-012 · LLM-034: **during** the generation, not after it. The
          footer says the same thing once the answer is finished, and until
          this existed the whole time the model was working — which is the
          whole time it matters — the window said nothing about where.

          LLM-033 rides along for anything that is not local: the excerpt
          count, the file count and the bytes, from the envelope's own
          disclosure, before the first token arrives. */}
      {turn.meta?.boundaryLabel && turn.running && (
        <p className={styles.boundary}>
          {/* An icon only when there is something to notice. A warning
              triangle on a local answer would make the ordinary case look
              like a problem. */}
          {turn.meta.boundary !== "local" && <Icon name="warning" size={11} />}
          {turn.meta.model} · {turn.meta.boundaryLabel}
          {turn.meta.destination && ` (${turn.meta.destination})`}
          {turn.meta.boundary !== "local" && turn.meta.bytes > 0 && (
            <span>
              {" · "}
              {turn.meta.excerpts}{" "}
              {turn.meta.excerpts === 1 ? "excerpt" : "excerpts"} from{" "}
              {turn.meta.files} {turn.meta.files === 1 ? "file" : "files"},{" "}
              {bytes(turn.meta.bytes)}
            </span>
          )}
        </p>
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
            {/* Same rule as the two above: it changes how the answer should be
                read, so it must be answerable without opening anything. A
                count of inexact sources is the difference between "quoted"
                and "recovered from a scan". */}
            {inexact(turn.sources) > 0 && (
              <span className={styles.sourcesInexact}>
                {inexact(turn.sources)} not exact
              </span>
            )}
          </summary>
          <ol className={styles.sourceList}>
            {turn.sources.map((s) => (
              <li key={s.id} id={`cite-${turn.id}-${s.id}`} className={styles.source}>
                <span className={styles.sourceId}>{s.id}</span>
                <div className={styles.sourceBody}>
                  {/* **The citation opens the file.** This was a `<span>`.
                      `path` and `line` crossed the IPC boundary on every
                      source and were dropped on the floor, and `openPath` —
                      which works, and which the Files and Search views both
                      call — was never called from the one screen the promise
                      was written for. Getting from a sentence to its source
                      meant reading a path off the screen, leaving for Search
                      and typing it again. GUI §11 asks for one action. */}
                  <button
                    type="button"
                    className={styles.sourceOpen}
                    onClick={(e) =>
                      void (e.shiftKey
                        ? revealInFileManager(s.path, s.relativePath)
                        : openInSystem(s.path, s.relativePath))
                    }
                    title={`${s.relativePath}${s.line === null ? "" : `:${s.line}`}\nClick to open · Shift-click to reveal in Finder`}
                  >
                    {/* Split so the **end** survives. `location` is
                        `path:line` in one string under `text-overflow:
                        ellipsis`, so on any real path the ellipsis ate the
                        `:line` — the part that makes it a citation rather
                        than a filename. The directory truncates; the file and
                        its line never do. */}
                    <span className={styles.sourceDir}>{dirOf(s.relativePath)}</span>
                    <span className={styles.sourceName}>
                      {nameOf(s.relativePath)}
                      {/* `line` alone could only ever say ":42". A PDF's page
                          and a spreadsheet's cell are the citations this
                          product is for, and both were rendering as a bare
                          filename here. */}
                      {where(s) !== null && <span className={styles.sourceLine}>{where(s)}</span>}
                    </span>
                  </button>
                  {/* Computed in Rust for every citation and rendered nowhere,
                      so an answer built from degraded OCR looked exactly like
                      one built from exact PDF spans. Renders nothing when the
                      provenance is `exact`, which is most of the time. */}
                  <ProvenanceBadge provenance={s.provenance} />
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
            // The one motion in the app `prefers-reduced-motion` did not reach.
            el?.scrollIntoView({
              block: "center",
              behavior: window.matchMedia("(prefers-reduced-motion: reduce)")
                .matches
                ? "auto"
                : "smooth",
            });
          }}
        />
      )}

      {/* Beside the answer, not instead of it. A notice means the provider
          could not honour something, or read something, or is about to hit a
          limit — the answer under it is real, so this is not the failure
          style. Held in state and never drawn would be the same silent drop
          the event exists to end. */}
      {turn.notices.map((n, i) => (
        <p className={styles.notice} key={`${n.code ?? "notice"}-${i}`}>
          <Icon name="warning" size={12} />
          <span>{n.message}</span>
        </p>
      ))}

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
            {turn.meta && `${turn.meta.model} · `}
            {turn.meta?.boundaryLabel && `${turn.meta.boundaryLabel} · `}
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
