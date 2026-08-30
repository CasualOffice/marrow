/**
 * A streamed answer: Markdown, diagrams, and previews.
 *
 * The model's output is untrusted — it is a guess about documents that may
 * themselves contain hostile text — so nothing here executes it in this page.
 * Markdown is sanitised; Mermaid is rendered by a library that produces SVG,
 * not script; and an `html` fence goes into a **sandboxed iframe with no
 * same-origin access**, which is the only way to show a page without becoming
 * it.
 */

import { useEffect, useMemo, useRef, useState } from "react";

import styles from "./Answer.module.css";
import { cx } from "../lib/cx";
import { linkCitations, parseAnswer, type Block } from "../lib/markdown";

/**
 * Mermaid is about 2 MB. Loaded the first time a diagram appears rather than
 * at startup, because most answers have none and launch must not wait on it.
 */
let mermaidPromise: Promise<typeof import("mermaid").default> | null = null;
function loadMermaid() {
  if (!mermaidPromise) {
    mermaidPromise = import("mermaid").then((m) => m.default);
  }
  return mermaidPromise;
}

/**
 * Resolve the design tokens to real colours.
 *
 * Mermaid rejects `var(--el2)` outright — it parses colours rather than
 * emitting them — so the values have to be read from the document at render
 * time. Reading them also means a diagram follows the theme instead of being
 * baked at first paint.
 */
function themeVariables(): Record<string, string> {
  const cs = getComputedStyle(document.documentElement);
  const v = (name: string, fallback: string) =>
    cs.getPropertyValue(name).trim() || fallback;
  // Only real colour tokens: `--el1`/`--el2` are elevation *shadows* and
  // mermaid parses what it is given rather than emitting it.
  const line = v("--line-strong", "#c9c9c2");
  const fg = v("--fg", "#1c1c19");
  return {
    background: "transparent",
    primaryColor: v("--sunken", "#f0efeb"),
    primaryTextColor: fg,
    primaryBorderColor: line,
    secondaryColor: v("--sunken", "#f4f3ef"),
    tertiaryColor: v("--sheet", "#fbfaf7"),
    lineColor: line,
    textColor: fg,
    mainBkg: v("--sunken", "#f0efeb"),
    nodeBorder: line,
    clusterBkg: v("--sunken", "#f4f3ef"),
    clusterBorder: v("--line", "#e2e1db"),
    edgeLabelBackground: v("--sheet", "#ffffff"),
    fontFamily: v("--sans", "system-ui, sans-serif"),
    fontSize: "13px",
  };
}

/** The theme a diagram was drawn for, so a toggle redraws it. */
function currentTheme(): string {
  const explicit = document.documentElement.getAttribute("data-theme");
  if (explicit) return explicit;
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

let diagramSeq = 0;

function Diagram({ source }: { source: string }) {
  const [svg, setSvg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const id = useMemo(() => `mmd-${(diagramSeq += 1)}`, []);
  const theme = useTheme();

  useEffect(() => {
    let live = true;
    // A diagram mid-stream is usually incomplete, so a parse failure is the
    // normal case rather than an error. It is only reported once the block has
    // stopped changing.
    const t = window.setTimeout(() => {
      loadMermaid()
        .then((m) => {
          m.initialize({
            startOnLoad: false,
            // `loose` would let a diagram carry click handlers, which is
            // exactly what untrusted output must not have.
            securityLevel: "strict",
            theme: "base",
            themeVariables: themeVariables(),
          });
          return m.render(`${id}-${theme}`, source);
        })
        .then((r) => {
          if (live) {
            setSvg(r.svg);
            setError(null);
          }
        })
        .catch((e: unknown) => {
          if (live) setError(e instanceof Error ? e.message : String(e));
        });
    }, 200);
    return () => {
      live = false;
      window.clearTimeout(t);
    };
  }, [id, source, theme]);

  if (error) {
    return (
      <figure className={styles.diagramFailed}>
        <figcaption>That diagram could not be drawn.</figcaption>
        <pre>{source}</pre>
        <p className={styles.diagramWhy}>{error}</p>
      </figure>
    );
  }
  if (!svg) {
    return <div className={cx(styles.diagram, styles.diagramPending)} aria-busy="true" />;
  }
  // The SVG came from Mermaid in `strict` mode, which strips script and event
  // handlers; it never contains the model's raw text as markup.
  return (
    <figure className={styles.diagram} dangerouslySetInnerHTML={{ __html: svg }} />
  );
}

/**
 * The last version of a source that stopped changing.
 *
 * Assigning `srcDoc` reloads the frame from scratch, so following the token
 * stream directly would mean a hundred reloads of a document that is
 * half-written in every one of them. Waiting for a pause costs a moment and
 * buys a frame that renders a page the model has at least finished a thought
 * in, once.
 */
function useQuiet(source: string, streaming: boolean): string {
  const [quiet, setQuiet] = useState(streaming ? "" : source);
  useEffect(() => {
    if (!streaming) {
      setQuiet(source);
      return;
    }
    const t = window.setTimeout(() => setQuiet(source), 500);
    return () => window.clearTimeout(t);
  }, [source, streaming]);
  return quiet;
}

/**
 * A generated page, shown without being trusted.
 *
 * `sandbox` without `allow-same-origin` gives the frame an opaque origin: no
 * access to this document, no cookies, no storage, no network under our
 * identity. Scripts are allowed because a page with none is not the thing the
 * user asked to see — but they run somewhere that cannot reach anything.
 *
 * The page leads, the markup follows. Someone who asked for a page wants to
 * look at the page; opening on a wall of angle brackets makes them do the
 * rendering in their head to find out whether the model understood them.
 */
function Preview({ source, streaming }: { source: string; streaming: boolean }) {
  const [showSource, setShowSource] = useState(false);
  const rendered = useQuiet(source, streaming);
  const sourceEl = useRef<HTMLPreElement>(null);
  /**
   * Whether the source block is still where we put it. Anything that grows a
   * scroller mid-stream can drag its offset along, and the user then meets the
   * document at a random line in its middle. We put it back — but only while
   * they have not scrolled it themselves, because overriding a deliberate
   * scroll is the more annoying of the two failures.
   */
  const atTop = useRef(true);

  useEffect(() => {
    if (!showSource) {
      atTop.current = true;
      return;
    }
    const el = sourceEl.current;
    if (el && atTop.current) el.scrollTop = 0;
  }, [showSource, source, streaming]);

  const lines = useMemo(() => source.split("\n").length, [source]);

  return (
    <figure className={styles.preview}>
      <figcaption className={styles.previewHead}>
        <div className={styles.previewTitle}>
          <span>Generated page</span>
          <span className={styles.previewSize}>
            {lines} {lines === 1 ? "line" : "lines"} of HTML
          </span>
          <button
            type="button"
            className={styles.previewToggle}
            onClick={() => setShowSource((s) => !s)}
          >
            {showSource ? "Run the page here" : "Show the HTML"}
          </button>
        </div>
        {/* The isolation and the destination, stated. A preview that says
            neither what it can reach nor where it will appear is a preview the
            user has to guess about. */}
        <span className={styles.previewNote}>
          {showSource
            ? "The HTML the model wrote. Run it to render the page below, sealed off from your files, your index and this window."
            : "Running below in a sealed frame — no access to your files, your index or this window."}
        </span>
      </figcaption>
      {showSource ? (
        <pre
          ref={sourceEl}
          className={styles.previewSource}
          onScroll={(e) => {
            atTop.current = e.currentTarget.scrollTop === 0;
          }}
        >
          {source}
        </pre>
      ) : rendered ? (
        <iframe
          className={styles.previewFrame}
          title="Generated page"
          sandbox="allow-scripts"
          srcDoc={rendered}
        />
      ) : (
        <div className={styles.previewPending} aria-busy="true">
          Writing the page…
        </div>
      )}
    </figure>
  );
}

function Prose({ html, citations }: { html: string; citations: ReadonlySet<string> }) {
  const linked = useMemo(() => linkCitations(html, citations), [html, citations]);
  return <div className={styles.prose} dangerouslySetInnerHTML={{ __html: linked }} />;
}

/** Re-renders on a theme change, so diagrams are never baked at first paint. */
function useTheme(): string {
  const [theme, setTheme] = useState(currentTheme);
  useEffect(() => {
    const update = () => setTheme(currentTheme());
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    media.addEventListener("change", update);
    const observer = new MutationObserver(update);
    observer.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme"],
    });
    return () => {
      media.removeEventListener("change", update);
      observer.disconnect();
    };
  }, []);
  return theme;
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
            return <Diagram key={i} source={b.source} />;
          case "html":
            return <Preview key={i} source={b.source} streaming={streaming} />;
        }
      })}
      {/* A caret while tokens are still arriving. It is the only motion on the
          page, and it stops the moment the answer does. */}
      {streaming && <span className={styles.caret} aria-hidden="true" />}
    </div>
  );
}
