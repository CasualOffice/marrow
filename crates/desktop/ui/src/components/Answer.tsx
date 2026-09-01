/**
 * A streamed answer: Markdown prose, plus a card for anything the model built
 * rather than wrote.
 *
 * The model's output is untrusted — it is a guess about documents that may
 * themselves contain hostile text — so nothing here executes it in this page.
 * Markdown is sanitised before it reaches the DOM. A `mermaid` or `html` fence
 * is not rendered here at all: it becomes an *artifact*, which opens in the
 * side panel, and every line of the machinery that renders one without trusting
 * it lives in `ArtifactPanel.tsx`.
 *
 * **A diagram is drawn here; a generated page is not.** That argument — "a
 * 340px viewport of a document that wants a screen, sitting in the middle of a
 * 62-character column" — is about a *page*, and was never about a flow chart.
 * Five boxes and four arrows are part of the sentence the answer is making,
 * and a card reading "Diagram · 9 lines" is a receipt for something the reader
 * could have been shown. So `html` keeps the card and the panel, and `mermaid`
 * renders in the flow.
 */

import { useEffect, useId, useMemo, useRef } from "react";

import styles from "./Answer.module.css";
import { cx } from "../lib/cx";
import { Icon } from "./Icon";
import { linkCitations, parseAnswer, type Block } from "../lib/markdown";
import {
  ARTIFACT_PANEL_ID,
  Diagram,
  artifactSummary,
  artifactTitle,
} from "./ArtifactPanel";
import { useUi, type ArtifactKind } from "../store";

function Prose({ html, citations }: { html: string; citations: ReadonlySet<string> }) {
  const linked = useMemo(() => linkCitations(html, citations), [html, citations]);
  return <div className={styles.prose} dangerouslySetInnerHTML={{ __html: linked }} />;
}

/**
 * The entry point to a generated page or diagram.
 *
 * `useId` rather than the block's position in the answer: it is stable for as
 * long as this card is mounted and unique across every answer in the thread,
 * which is exactly what the panel needs to tell "the artifact I am showing has
 * more tokens" from "a different artifact was opened".
 */
function ArtifactCard({
  kind,
  source,
  streaming,
}: {
  kind: ArtifactKind;
  source: string;
  streaming: boolean;
}) {
  const key = useId();
  const openKey = useUi((s) => s.artifact?.key ?? null);
  const open = useUi((s) => s.openArtifact);
  const isOpen = openKey === key;
  const button = useRef<HTMLButtonElement>(null);

  const title = useMemo(() => artifactTitle(kind, source), [kind, source]);
  const summary = useMemo(() => artifactSummary(kind, source), [kind, source]);

  /* While this is the one on show, the panel follows the stream. Opening an
     artifact mid-answer and watching it stop growing would be worse than not
     being able to open it at all. */
  useEffect(() => {
    if (isOpen) {
      useUi.getState().refreshArtifact({ key, kind, title, source, streaming });
    }
  }, [isOpen, key, kind, title, source, streaming]);

  /* If the answer that produced it goes away — Retry drops the turn and asks
     again — the panel must not be left showing a page that nothing on screen
     produced any more. */
  useEffect(
    () => () => {
      const ui = useUi.getState();
      if (ui.artifact?.key === key) ui.closeArtifact();
    },
    [key],
  );

  /* Closing hands focus back to the card it was opened from, so the keyboard
     lands where the eye already is. Only on a real close: when a *different*
     artifact replaced this one, focus belongs to the panel that now holds it. */
  const wasOpen = useRef(false);
  useEffect(() => {
    if (wasOpen.current && !isOpen && useUi.getState().artifact === null) {
      button.current?.focus();
    }
    wasOpen.current = isOpen;
  }, [isOpen]);

  return (
    <button
      ref={button}
      type="button"
      className={cx(styles.card, isOpen && styles.cardOn)}
      aria-pressed={isOpen}
      onClick={() => {
        if (isOpen) {
          // Already on show, so the only useful thing left is to take you
          // there rather than to re-open what is in front of you.
          document.getElementById(ARTIFACT_PANEL_ID)?.focus();
          return;
        }
        open({ key, kind, title, source, streaming });
      }}
    >
      <span className={styles.cardText}>
        <span className={styles.cardTitle}>{title}</span>
        <span className={styles.cardMeta}>{summary}</span>
      </span>
      <span className={styles.cardOpen}>
        {isOpen ? "In the panel" : "Open"}
        <Icon name="arrowRight" size={12} />
      </span>
    </button>
  );
}

export function Answer({
  text,
  citations,
  streaming,
  onCite,
}: {
  text: string;
  citations: ReadonlySet<string>;
  streaming: boolean;
  onCite: (id: string) => void;
}) {
  const blocks = useMemo<Block[]>(() => parseAnswer(text), [text]);

  return (
    <div
      className={styles.answer}
      onClick={(e) => {
        // Citations navigate inside the app; they are not links to anywhere.
        const el = (e.target as HTMLElement).closest<HTMLElement>("[data-cite]");
        if (el) {
          e.preventDefault();
          onCite(el.dataset.cite ?? "");
        }
      }}
    >
      {blocks.map((b, i) => {
        switch (b.kind) {
          case "markdown":
            return <Prose key={i} html={b.html} citations={citations} />;
          case "mermaid":
            /*
             * **Inline, in the flow.** A diagram is part of the sentence the
             * answer is making, and a card that says "Diagram · 9 lines" is a
             * receipt for something the reader could simply have been shown.
             *
             * While it streams there is nothing to draw: a mermaid fence is a
             * syntax error for most of its life, and rendering every keystroke
             * would flicker an error box through the prose. The card stands in
             * until the fence closes, which is the same thing `Diagram` would
             * have shown anyway and costs no layout change when it arrives.
             */
            return streaming ? (
              <ArtifactCard key={i} kind="mermaid" source={b.source} streaming />
            ) : (
              <div key={i} className={styles.diagram}>
                <Diagram source={b.source} name={artifactTitle("mermaid", b.source)} />
              </div>
            );
          case "html":
            return <ArtifactCard key={i} kind="html" source={b.source} streaming={streaming} />;
        }
      })}
      {/* A caret while tokens are still arriving. It is the only motion on the
          page, and it stops the moment the answer does. */}
      {streaming && <span className={styles.caret} aria-hidden="true" />}
    </div>
  );
}
