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

import styles from "./AskView.module.css";
import { cx } from "../lib/cx";
import { bytes, duration } from "../lib/format";
import { Answer } from "./Answer";
import { Icon } from "./Icon";
import { Kbd } from "./Kbd";
import {
  ask,
  cancelAsk,
  forgetConversation,
  type AskEvent,
  type Citation,
  type ExcludedSource,
  type PriorTurn,
} from "../api";
import { useUi } from "../store";

interface Usage {
  readonly promptTokens: number;
  readonly outputTokens: number;
  readonly thinkingTokens: number;
  readonly cachedPrefixTokens: number;
  readonly stopReason: string;
  readonly elapsedMs: number;
}

interface Turn {
  readonly id: string;
  readonly question: string;
  readonly thorough: boolean;
  answer: string;
  thinking: string;
  sources: readonly Citation[];
  excluded: readonly ExcludedSource[];
  meta: { boundary: string; model: string; bytes: number } | null;
  usage: Usage | null;
  failure: { code: string; message: string } | null;
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
    meta: null,
    usage: null,
    failure: null,
    running: true,
  };
}

function conversationId() {
  return `c-${Date.now().toString(36)}-${Math.random().toString(36).slice(2, 8)}`;
}

export function AskView() {
  const [conversation, setConversation] = useState(conversationId);
  const [turns, setTurns] = useState<Turn[]>([]);
  const [question, setQuestion] = useState("");
  const [thorough, setThorough] = useState(false);
  const [running, setRunning] = useState(false);
  const askId = useRef<string | null>(null);
  const scroller = useRef<HTMLDivElement>(null);
  const field = useRef<HTMLTextAreaElement>(null);
  const stickToBottom = useRef(true);
  const setView = useUi((s) => s.setView);

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

  const send = useCallback(
    async (text: string, mode: boolean) => {
      const q = text.trim();
      if (!q || running) return;

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

      const onEvent = (e: AskEvent) => {
        switch (e.kind) {
          case "sources":
            update(turn.id, (t) => ({
              ...t,
              sources: e.hits,
              excluded: e.excluded,
              meta: { boundary: e.boundary, model: e.model, bytes: e.bytes },
            }));
            break;
          case "token":
            update(turn.id, (t) => ({ ...t, answer: t.answer + e.text }));
            break;
          case "thinking":
            update(turn.id, (t) => ({ ...t, thinking: t.thinking + e.text }));
            break;
          case "done":
            update(turn.id, (t) => ({ ...t, usage: e }));
            break;
          case "failed":
            update(turn.id, (t) => ({
              ...t,
              failure: { code: e.code, message: e.message },
            }));
            break;
        }
      };

      try {
        askId.current = await ask(
          { conversation, question: q, history, thorough: mode },
          onEvent,
        );
      } catch (e) {
        update(turn.id, (t) => ({
          ...t,
          failure: {
            code: "UI_UNEXPECTED",
            message: e instanceof Error ? e.message : String(e),
          },
        }));
      } finally {
        update(turn.id, (t) => ({ ...t, running: false }));
        setRunning(false);
        askId.current = null;
        field.current?.focus();
      }
    },
    [conversation, running, turns, update],
  );

  const stop = useCallback(() => {
    if (askId.current) void cancelAsk(askId.current);
  }, []);

  const clear = useCallback(() => {
    void forgetConversation(conversation);
    setConversation(conversationId());
    setTurns([]);
    setQuestion("");
    field.current?.focus();
  }, [conversation]);

  const retry = useCallback(
    (t: Turn) => {
      // Drop this turn and everything after it, then ask again — the same
      // shape as editing a message, and it keeps the history consistent.
      setTurns((all) => all.slice(0, all.findIndex((x) => x.id === t.id)));
      void send(t.question, t.thorough);
    },
    [send],
  );

  return (
    <section className={styles.view} aria-label="Ask">
      <div className={styles.scroll} ref={scroller} onScroll={onScroll}>
        {turns.length === 0 ? (
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
              stop();
            }
          }}
        />
        <div className={styles.controls}>
          {/* GEN-012: the switch is visible before the request is sent, next
              to what it costs, so the trade is legible while choosing. */}
          <div className={styles.modes} role="radiogroup" aria-label="Answer mode">
            {([false, true] as const).map((t) => (
              <button
                key={String(t)}
                type="button"
                role="radio"
                aria-checked={thorough === t}
                className={cx(styles.mode, thorough === t && styles.modeOn)}
                onClick={() => setThorough(t)}
                disabled={running}
              >
                <span className={styles.modeName}>{t ? "Thorough" : "Fast"}</span>
                <span className={styles.modeWhy}>
                  {t ? "reasons first, slower" : "straight answer"}
                </span>
              </button>
            ))}
          </div>
          <span className={styles.grow} />
          {turns.length > 0 && !running && (
            <button type="button" className={styles.ghost} onClick={clear}>
              New
            </button>
          )}
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
            {turn.meta && (
              <span className={styles.sourcesBytes}>{bytes(turn.meta.bytes)} of context</span>
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

      {(turn.answer || turn.running) && (
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
