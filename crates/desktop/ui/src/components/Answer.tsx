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
 * A generated page, shown without being trusted.
 *
 * `sandbox` without `allow-same-origin` gives the frame an opaque origin: no
 * access to this document, no cookies, no storage, no network under our
 * identity. Scripts are allowed because a page with none is not the thing the
 * user asked to see — but they run somewhere that cannot reach anything.
 */
function Preview({ source }: { source: string }) {
  const [open, setOpen] = useState(false);
  const frame = useRef<HTMLIFrameElement>(null);

  return (
    <figure className={styles.preview}>
      <figcaption className={styles.previewHead}>
        <span>Generated page</span>
        <span className={styles.previewNote}>
          runs isolated — no access to your files or this window
        </span>
        <button type="button" className={styles.previewToggle} onClick={() => setOpen((o) => !o)}>
          {open ? "Show source" : "Run it"}
        </button>
      </figcaption>
      {open ? (
        <iframe
          ref={frame}
          className={styles.previewFrame}
          title="Generated page"
          sandbox="allow-scripts"
          srcDoc={source}
        />
      ) : (
        <pre className={styles.previewSource}>{source}</pre>
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
            return <Preview key={i} source={b.source} />;
        }
      })}
      {/* A caret while tokens are still arriving. It is the only motion on the
          page, and it stops the moment the answer does. */}
      {streaming && <span className={styles.caret} aria-hidden="true" />}
    </div>
  );
}
