/**
 * The artifact panel — the generated thing, opened beside the conversation.
 *
 * An answer that writes a page or a diagram has produced two things, and they
 * want opposite treatment. The prose is read once, top to bottom, in a narrow
 * measure. The artefact is looked at, scrolled, compared back against the
 * question, and often copied out — and it was being given 340px inside a
 * 62-character column to be all of that in, which is enough to see that a page
 * exists and not enough to see what is on it. Here it gets the full height of
 * the sheet and a width the reader sets, and the conversation narrows beside it
 * instead of vanishing under it.
 *
 * The model's output is untrusted — it is a guess about documents that may
 * themselves contain hostile text — so nothing in this file executes it in this
 * page. Mermaid is asked for SVG in `strict` mode, and an `html` fence goes into
 * a **sandboxed iframe with no same-origin access**, which is the only way to
 * show a page without becoming it.
 */

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";

import styles from "./ArtifactPanel.module.css";
import { cx } from "../lib/cx";
import { Icon } from "./Icon";
import { Kbd } from "./Kbd";
import {
  ARTIFACT_W_MAX,
  ARTIFACT_W_MIN,
  CONVERSATION_W_MIN,
  useUi,
  type Artifact,
  type ArtifactKind,
} from "../store";

/**
 * There is at most one panel, ever — opening a second artifact replaces the
 * first — so it can afford a fixed id, and a card that is already open uses it
 * to move focus here instead of re-opening what is in front of you.
 */
export const ARTIFACT_PANEL_ID = "artifact-panel";

/* ── naming the thing ─────────────────────────────────────────────────────── */

/**
 * Mermaid's first keyword, spelled the way a person would say it. A panel
 * header that reads `flowchart TD` is showing the reader the syntax when what
 * they wanted was the noun.
 */
const DIAGRAM_NAMES: Readonly<Record<string, string>> = {
  graph: "Graph",
  flowchart: "Flowchart",
  sequencediagram: "Sequence diagram",
  classdiagram: "Class diagram",
  statediagram: "State diagram",
  "statediagram-v2": "State diagram",
  erdiagram: "Entity diagram",
  journey: "User journey",
  gantt: "Gantt chart",
  pie: "Pie chart",
  mindmap: "Mind map",
  timeline: "Timeline",
  gitgraph: "Git graph",
  quadrantchart: "Quadrant chart",
  sankey: "Sankey diagram",
  "sankey-beta": "Sankey diagram",
  xychart: "Chart",
  "xychart-beta": "Chart",
};

/**
 * What to call it on the card and in the panel header.
 *
 * A generated page nearly always names itself, in a `<title>` or in its first
 * heading, and that name is the only thing that distinguishes two artefacts in
 * one conversation. Read as text and rendered as text: this never reaches the
 * DOM as markup.
 */
export function artifactTitle(kind: ArtifactKind, source: string): string {
  if (kind === "mermaid") {
    const first = source
      .split("\n")
      .map((l) => l.trim())
      .find((l) => l !== "" && !l.startsWith("%%"));
    const word = /^[a-zA-Z-]+/.exec(first ?? "")?.[0]?.toLowerCase() ?? "";
    return DIAGRAM_NAMES[word] ?? "Diagram";
  }
  const title = /<title[^>]*>([\s\S]*?)<\/title>/i.exec(source)?.[1];
  const heading = /<h1[^>]*>([\s\S]*?)<\/h1>/i.exec(source)?.[1];
  const raw = (title ?? heading ?? "").replace(/<[^>]*>/g, "").trim();
  const clean = raw.replace(/\s+/g, " ").slice(0, 80);
  return clean === "" ? "Generated page" : clean;
}

/** The size of it, so the reader knows what opening it will cost them. */
export function artifactSummary(kind: ArtifactKind, source: string): string {
  const noun = kind === "html" ? "Generated page" : "Diagram";
  const body = source.trim();
  if (body === "") return `${noun} · still being written`;
  const lines = body.split("\n").length;
  const unit = kind === "html" ? "lines of HTML" : "lines";
  return `${noun} · ${lines} ${lines === 1 ? unit.replace("lines", "line") : unit}`;
}

/* ── diagrams ─────────────────────────────────────────────────────────────── */

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
    return <div className={styles.pending} aria-busy="true">Drawing…</div>;
  }
  // The SVG came from Mermaid in `strict` mode, which strips script and event
  // handlers; it never contains the model's raw text as markup.
  return (
    <div className={styles.diagram} dangerouslySetInnerHTML={{ __html: svg }} />
  );
}

/* ── generated pages ──────────────────────────────────────────────────────── */

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
 */
function Page({ source, streaming }: { source: string; streaming: boolean }) {
  const rendered = useQuiet(source, streaming);
  if (!rendered) {
    return (
      <div className={styles.pending} aria-busy="true">
        Writing the page…
      </div>
    );
  }
  return (
    <iframe
      className={styles.frame}
      title="Generated page"
      sandbox="allow-scripts"
      srcDoc={rendered}
    />
  );
}

/** The markup the model wrote, as text. */
function Source({ source, streaming }: { source: string; streaming: boolean }) {
  const el = useRef<HTMLPreElement>(null);
  /**
   * Whether the source block is still where we put it. Anything that grows a
   * scroller mid-stream can drag its offset along, and the user then meets the
   * document at a random line in its middle. We put it back — but only while
   * they have not scrolled it themselves, because overriding a deliberate
   * scroll is the more annoying of the two failures.
   */
  const atTop = useRef(true);

  useEffect(() => {
    const node = el.current;
    if (node && atTop.current) node.scrollTop = 0;
  }, [source, streaming]);

  return (
    <pre
      ref={el}
      className={cx("selectable", styles.source)}
      onScroll={(e) => {
        atTop.current = e.currentTarget.scrollTop === 0;
      }}
    >
      {source}
    </pre>
  );
}

/* ── the panel ────────────────────────────────────────────────────────────── */

/** The widest the panel may be given the room the sheet actually has. */
function maxWidth(available: number): number {
  return Math.max(
    ARTIFACT_W_MIN,
    Math.min(ARTIFACT_W_MAX, available - CONVERSATION_W_MIN),
  );
}

export function ArtifactPanel() {
  const artifact = useUi((s) => s.artifact);
  if (!artifact) return null;
  // Keyed on the artifact, so opening a different one is a fresh frame and a
  // fresh scroll position rather than the old one with new content poured in.
  return <Panel key={artifact.key} artifact={artifact} />;
}

function Panel({ artifact }: { artifact: Artifact }) {
  const mode = useUi((s) => s.artifactMode);
  const setMode = useUi((s) => s.setArtifactMode);
  const close = useUi((s) => s.closeArtifact);
  const width = useUi((s) => s.artifactWidth);
  const setWidth = useUi((s) => s.setArtifactWidth);

  const panel = useRef<HTMLElement>(null);
  const [copied, setCopied] = useState(false);
  /** The left edge of the column the panel is resizing within, while dragging. */
  const dragFrom = useRef<{ right: number; available: number } | null>(null);
  const [dragging, setDragging] = useState(false);

  const clamp = useCallback(
    (next: number, available: number) =>
      Math.round(Math.min(Math.max(next, ARTIFACT_W_MIN), maxWidth(available))),
    [],
  );

  /*
   * The window is not the only thing that changes the room available: so does
   * collapsing the sidebar. Without this the panel keeps a width that was legal
   * when it was set and is now most of a small window, and the conversation is
   * squeezed to nothing by a drag the user made ten minutes ago.
   */
  useEffect(() => {
    const host = panel.current?.parentElement;
    if (!host) return;
    const ro = new ResizeObserver(() => {
      const available = host.clientWidth;
      // Below the takeover threshold the stored width is not being used for
      // anything, and squeezing it to the minimum here would mean a window
      // briefly made narrow costs the reader the width they chose.
      if (available < ARTIFACT_W_MIN + CONVERSATION_W_MIN) return;
      const store = useUi.getState();
      const capped = clamp(store.artifactWidth, available);
      if (capped !== store.artifactWidth) store.setArtifactWidth(capped);
    });
    ro.observe(host);
    return () => ro.disconnect();
  }, [clamp]);

  /* Opening moves focus into the panel: it is the thing that just appeared, and
     leaving focus behind in the thread means Tab walks the whole conversation
     before it reaches the control that closes what is in front of you. */
  useEffect(() => {
    panel.current?.focus();
  }, []);

  const onGripDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    const host = panel.current?.parentElement;
    if (!host) return;
    e.preventDefault();
    const rect = host.getBoundingClientRect();
    dragFrom.current = { right: rect.right, available: rect.width };
    setDragging(true);
    e.currentTarget.setPointerCapture(e.pointerId);
  };

  const onGripMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    const from = dragFrom.current;
    if (!from) return;
    setWidth(clamp(from.right - e.clientX, from.available));
  };

  const endDrag = () => {
    dragFrom.current = null;
    setDragging(false);
  };

  /** The keyboard equivalent of the drag (GUI §11). */
  const onGripKey = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    const host = panel.current?.parentElement;
    if (!host) return;
    const available = host.clientWidth;
    const step = e.shiftKey ? 96 : 24;
    // The panel is on the right, so its edge moving left makes it wider.
    if (e.key === "ArrowLeft") setWidth(clamp(width + step, available));
    else if (e.key === "ArrowRight") setWidth(clamp(width - step, available));
    else if (e.key === "Home") setWidth(clamp(ARTIFACT_W_MIN, available));
    else if (e.key === "End") setWidth(clamp(ARTIFACT_W_MAX, available));
    else return;
    e.preventDefault();
  };

  const summary = artifactSummary(artifact.kind, artifact.source);

  return (
    <aside
      ref={panel}
      id={ARTIFACT_PANEL_ID}
      tabIndex={-1}
      className={styles.panel}
      style={{ "--artifact-w": `${width}px` } as CSSProperties}
      aria-label={`${artifact.title} — generated by the model`}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.preventDefault();
          close();
        }
      }}
    >
      {/*
        The drag handle sits on the seam and is a `separator` rather than a
        decoration, so it is in the tab order and the arrow keys move it. A
        resize that only a mouse can perform is a control half the users of this
        window do not have.
      */}
      <div
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize the panel"
        aria-valuenow={width}
        aria-valuemin={ARTIFACT_W_MIN}
        aria-valuemax={ARTIFACT_W_MAX}
        aria-valuetext={`${width} pixels wide`}
        tabIndex={0}
        className={cx(styles.grip, dragging && styles.gripOn)}
        onPointerDown={onGripDown}
        onPointerMove={onGripMove}
        onPointerUp={endDrag}
        onLostPointerCapture={endDrag}
        onKeyDown={onGripKey}
      />

      <header className={styles.head}>
        <div className={styles.titles}>
          <h2 className={styles.title} title={artifact.title}>
            {artifact.title}
          </h2>
          <p className={styles.sub}>{summary}</p>
        </div>

        <div className={styles.modes} role="radiogroup" aria-label="How to show it">
          {(["preview", "source"] as const).map((m) => (
            <button
              key={m}
              type="button"
              role="radio"
              aria-checked={mode === m}
              className={cx(styles.mode, mode === m && styles.modeOn)}
              onClick={() => setMode(m)}
            >
              {m === "preview" ? "Preview" : "Source"}
            </button>
          ))}
        </div>

        <button
          type="button"
          className={styles.ghost}
          onClick={() => {
            void navigator.clipboard?.writeText(artifact.source);
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1400);
          }}
        >
          {copied ? "Copied" : "Copy"}
        </button>

        <button
          type="button"
          className={styles.iconBtn}
          aria-label="Close the panel"
          onClick={close}
        >
          <Icon name="close" size={13} />
        </button>
      </header>

      {/* The isolation, stated where it is being relied on. A frame that says
          nothing about what it can reach is a frame the user has to guess
          about — and the guess people make about a rendered page is that it is
          part of the app. */}
      {mode === "preview" && artifact.kind === "html" && (
        <p className={styles.seal}>
          Running in a sealed frame — no access to your files, your index or this
          window.
        </p>
      )}

      <div className={styles.body}>
        {mode === "source" ? (
          <Source source={artifact.source} streaming={artifact.streaming} />
        ) : artifact.kind === "html" ? (
          <Page source={artifact.source} streaming={artifact.streaming} />
        ) : (
          <div className={styles.diagramScroll}>
            <Diagram source={artifact.source} />
          </div>
        )}
      </div>

      <footer className={styles.foot}>
        <Kbd>Esc</Kbd> closes this
      </footer>
    </aside>
  );
}
