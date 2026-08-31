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
  type WheelEvent,
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

/* ── getting it back out ─────────────────────────────────────────────────────
 *
 * An artefact you cannot take anywhere is a screenshot with extra steps. The
 * diagram leaves as SVG or PNG, the generated page as `.html`.
 */

/**
 * Hand a finished file to the user.
 *
 * **Deliberately a browser download and not a save dialog.**
 * `crates/desktop/capabilities/main.json` grants this WebView `core:default`
 * and nothing else, because SEC-012 is that the window rendering model output
 * has no filesystem affordance at all — only named Rust commands. Neither
 * `@tauri-apps/plugin-dialog` nor `@tauri-apps/plugin-fs` is a dependency, and
 * adding one would mean handing filesystem permissions to precisely the window
 * that must not have them, to save three files. The cost is that the user does
 * not choose the folder; the download lands wherever the WebView puts them.
 */
function download(name: string, blob: Blob): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.rel = "noopener";
  // WebKit ignores a click on a node that is not in the document, so it has to
  // be attached for the length of the call and gone immediately after.
  document.body.appendChild(a);
  a.click();
  a.remove();
  // Revoking in the same turn cancels the download it was created for. The
  // read has long since started by the time this fires.
  window.setTimeout(() => URL.revokeObjectURL(url), 30_000);
}

/**
 * The artefact's title as a filename.
 *
 * Titles come from the model, so they contain slashes, colons, newlines and
 * emoji — and `a.download` takes a *filename*, not a path: a title with a `/`
 * in it is silently truncated to whatever followed the last one.
 */
function filename(title: string, extension: string): string {
  const stem = title
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 60);
  return `${stem === "" ? "artifact" : stem}.${extension}`;
}

/**
 * UTF-8 safe base64.
 *
 * `btoa` throws on the first character outside Latin-1, which for a diagram is
 * any label containing an em dash.
 */
function base64(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  // In chunks: spreading a whole large diagram into `fromCharCode` overflows
  // the argument list, and a large diagram is when an export matters most.
  for (let i = 0; i < bytes.length; i += 0x8000) {
    binary += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

interface Box {
  x: number;
  y: number;
  w: number;
  h: number;
}

function parseViewBox(svg: Element): Box | null {
  const [x, y, w, h] = (svg.getAttribute("viewBox") ?? "")
    .trim()
    .split(/[\s,]+/)
    .map(Number);
  if (x === undefined || y === undefined || w === undefined || h === undefined) {
    return null;
  }
  if (!Number.isFinite(x) || !Number.isFinite(y) || !(w > 0) || !(h > 0)) {
    return null;
  }
  return { x, y, w, h };
}

/**
 * The size to give a drawing that is about to leave the app.
 *
 * Mermaid sizes its SVG for a *container*: `width="100%"`, with the real extent
 * only in the `viewBox`. That is right on screen and wrong in a file — an SVG
 * with no intrinsic size opens at whatever the viewer guesses, and `drawImage`
 * of one rasterises to an empty canvas rather than failing.
 */
function extent(svg: Element): Box | null {
  const view = parseViewBox(svg);
  const px = (raw: string | null) => {
    // A percentage is a fraction of a container the file will not have.
    if (raw === null || raw.includes("%")) return NaN;
    const n = Number.parseFloat(raw);
    return Number.isFinite(n) && n > 0 ? n : NaN;
  };
  const w = px(svg.getAttribute("width"));
  const h = px(svg.getAttribute("height"));
  if (Number.isFinite(w) && Number.isFinite(h)) {
    return { x: view?.x ?? 0, y: view?.y ?? 0, w, h };
  }
  return view;
}

/** The colour a diagram was drawn against, so it goes with it. */
function paperColour(): string {
  return (
    getComputedStyle(document.documentElement).getPropertyValue("--sheet").trim() ||
    "#ffffff"
  );
}

/**
 * Mermaid's markup, turned into a file that stands on its own.
 *
 * `DOMParser` in `image/svg+xml` mode builds a detached tree and runs nothing:
 * no script, no subresource loads, no reference to this document. That is the
 * only acceptable way to touch this markup — it was drawn from model output,
 * and an export is not a reason to start executing it.
 */
function standalone(markup: string): { xml: string; box: Box } {
  const doc = new DOMParser().parseFromString(markup, "image/svg+xml");
  const svg = doc.documentElement;
  if (doc.getElementsByTagName("parsererror").length > 0 || svg.localName !== "svg") {
    throw new Error("the drawing could not be read back");
  }

  const box = extent(svg);
  if (!box) throw new Error("the drawing has no size to export at");

  svg.setAttribute("xmlns", "http://www.w3.org/2000/svg");
  svg.setAttribute("width", String(Math.round(box.w)));
  svg.setAttribute("height", String(Math.round(box.h)));
  svg.setAttribute("viewBox", `${box.x} ${box.y} ${box.w} ${box.h}`);

  // Mermaid pins `max-width` inline so a drawing fits the panel it is in. Left
  // in a file, that is the one rule that makes it open at the wrong size in
  // every viewer that is not this panel.
  const style = svg.getAttribute("style");
  if (style !== null) {
    svg.setAttribute("style", style.replace(/max-width\s*:[^;]*;?/gi, "").trim());
  }

  /*
   * The paper, painted in.
   *
   * Diagrams are drawn on `background: transparent` so they sit on the panel.
   * Exported that way a dark-theme diagram is pale text on nothing: it lands in
   * a document as an apparently empty rectangle, and the user has no way to
   * tell that from a failed export.
   */
  const paper = doc.createElementNS("http://www.w3.org/2000/svg", "rect");
  paper.setAttribute("x", String(box.x));
  paper.setAttribute("y", String(box.y));
  paper.setAttribute("width", String(box.w));
  paper.setAttribute("height", String(box.h));
  paper.setAttribute("fill", paperColour());
  svg.insertBefore(paper, svg.firstChild);

  const xml = new XMLSerializer().serializeToString(doc);
  return { xml: `<?xml version="1.0" encoding="UTF-8"?>\n${xml}`, box };
}

/**
 * Twice the drawing's own size, so the labels are still legible when the image
 * is dropped into a document at its natural size and then looked at on a
 * retina display. Anything less and a PNG export is a worse copy of the SVG for
 * no reason.
 */
const PNG_RATIO = 2;

async function rasterise(xml: string, box: Box): Promise<Blob> {
  const image = new Image();
  await new Promise<void>((resolve, reject) => {
    image.onload = () => resolve();
    image.onerror = () => reject(new Error("the drawing could not be rasterised"));
    // The CSP in `tauri.conf.json` is `img-src 'self' data:` — an object URL
    // here is blocked outright, so the drawing goes in base64-encoded even
    // though the blob would be cheaper.
    image.src = `data:image/svg+xml;base64,${base64(xml)}`;
  });

  const canvas = document.createElement("canvas");
  canvas.width = Math.max(1, Math.round(box.w * PNG_RATIO));
  canvas.height = Math.max(1, Math.round(box.h * PNG_RATIO));
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("this window has no 2D canvas");
  ctx.drawImage(image, 0, 0, canvas.width, canvas.height);

  return await new Promise<Blob>((resolve, reject) => {
    canvas.toBlob(
      (blob) =>
        blob ? resolve(blob) : reject(new Error("the image could not be encoded")),
      "image/png",
    );
  });
}

/**
 * A transient line of text under a control, and the reason exports never throw.
 *
 * A save that fails must not take the artefact with it: the panel still has the
 * thing in it, and losing the diagram because the *copy* of it failed is by
 * some distance the worse of the two outcomes.
 */
function useNotice(): [string | null, (text: string) => void] {
  const [notice, setNotice] = useState<string | null>(null);
  const timer = useRef(0);
  useEffect(() => () => window.clearTimeout(timer.current), []);
  const say = useCallback((text: string) => {
    setNotice(text);
    window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => setNotice(null), 3000);
  }, []);
  return [notice, say];
}

/** What went wrong, said as a cause rather than as a stack. */
function why(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

let diagramSeq = 0;

/** How far a diagram may be shrunk or enlarged, and by how much per step. */
const MIN_ZOOM = 0.25;
const MAX_ZOOM = 6;
const ZOOM_STEP = 1.25;

const clampZoom = (z: number) => Math.min(MAX_ZOOM, Math.max(MIN_ZOOM, z));

function Diagram({ source, name }: { source: string; name: string }) {
  const [svg, setSvg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [zoom, setZoom] = useState(1);
  const [saving, setSaving] = useState(false);
  const [notice, say] = useNotice();
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
            /*
             * Labels as SVG `<text>`, not HTML inside a `<foreignObject>`.
             *
             * WebKit draws nothing inside a foreignObject when an SVG is used
             * as an *image* — which is exactly how the PNG export rasterises
             * it. With mermaid's default the exported picture came out as
             * boxes and arrows with every label missing, and the failure is
             * silent: the file saves, it is just wrong. Turning it off also
             * means the file on disk is the drawing on screen rather than a
             * second rendering path that can drift from it.
             */
            htmlLabels: false,
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

  /**
   * **A diagram that cannot be enlarged is a picture of a diagram.**
   *
   * The SVG was pinned at `max-width: 100%`, so a dense graph was squeezed to
   * the panel's width and the labels became unreadable — and because nothing
   * ever exceeded the container, the surrounding `overflow: auto` had nothing
   * to scroll either. Width, not `transform: scale`, precisely so that zooming
   * changes the laid-out size and the scroller can then pan over it.
   */
  const nudge = useCallback((dir: number) => {
    setZoom((z) => clampZoom(z * (dir > 0 ? ZOOM_STEP : 1 / ZOOM_STEP)));
  }, []);

  const onWheel = useCallback((e: WheelEvent<HTMLDivElement>) => {
    // Only with a modifier. A plain wheel over a diagram is someone scrolling
    // the panel past it, and stealing that would be its own bug.
    if (!e.ctrlKey && !e.metaKey) return;
    e.preventDefault();
    setZoom((z) => clampZoom(z * Math.exp(-e.deltaY / 400)));
  }, []);

  /**
   * Save the drawing as it is on screen.
   *
   * The rendered SVG is already in state, so neither format re-runs mermaid and
   * neither one depends on the zoom: what leaves is the drawing at its own
   * size, not at the size the reader happens to be looking at it.
   */
  const save = useCallback(
    (format: "svg" | "png") => {
      if (svg === null || saving) return;
      setSaving(true);
      void (async () => {
        try {
          const { xml, box } = standalone(svg);
          const file = filename(name, format);
          if (format === "svg") {
            download(file, new Blob([xml], { type: "image/svg+xml;charset=utf-8" }));
          } else {
            download(file, await rasterise(xml, box));
          }
          say(`Saved ${file}`);
        } catch (e: unknown) {
          say(`Could not save it — ${why(e)}.`);
        } finally {
          setSaving(false);
        }
      })();
    },
    [name, saving, say, svg],
  );

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
    <>
      <div
        className={styles.diagram}
        style={{ "--zoom": zoom } as CSSProperties}
        onWheel={onWheel}
        dangerouslySetInnerHTML={{ __html: svg }}
      />
      <div className={styles.controls}>
        {/* Always in the tree, never conditionally mounted: a live region that
            appears at the same moment as its text is one a screen reader has
            no chance to announce. */}
        <p className={styles.notice} role="status">{notice ?? ""}</p>
        <div className={styles.zoom} role="group" aria-label="Diagram controls">
          <button type="button" onClick={() => nudge(-1)} disabled={zoom <= MIN_ZOOM}
                  title="Zoom out" aria-label="Zoom out">−</button>
          <button type="button" className={styles.zoomLevel} onClick={() => setZoom(1)}
                  title="Reset to fit">{Math.round(zoom * 100)}%</button>
          <button type="button" onClick={() => nudge(1)} disabled={zoom >= MAX_ZOOM}
                  title="Zoom in" aria-label="Zoom in">+</button>
          {/* Same pill: looking at the diagram and taking it away are one job,
              and a second floating group would say they were two. */}
          <span className={styles.sep} aria-hidden="true" />
          <button type="button" className={styles.saveBtn} onClick={() => save("svg")}
                  disabled={saving} title="Save the drawing as an SVG file"
                  aria-label="Save as SVG">SVG</button>
          <button type="button" className={styles.saveBtn} onClick={() => save("png")}
                  disabled={saving} title={`Save the drawing as a PNG at ${PNG_RATIO}×`}
                  aria-label="Save as PNG">PNG</button>
        </div>
      </div>
    </>
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
  const [saved, setSaved] = useState<"ok" | "failed" | null>(null);
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

  /**
   * The generated page, written out as a file.
   *
   * Straight from the string we already hold — the sandboxed frame is never
   * asked for its DOM, so exporting the page never involves running it outside
   * the sandbox. `text/html` rather than an octet stream because the file is
   * going to be opened in a browser, and a download the OS cannot type is one
   * the user has to rename before they can look at it.
   */
  const savePage = () => {
    try {
      download(
        filename(artifact.title, "html"),
        new Blob([artifact.source], { type: "text/html;charset=utf-8" }),
      );
      setSaved("ok");
    } catch {
      setSaved("failed");
    }
    window.setTimeout(() => setSaved(null), 2400);
  };

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

        {/* Only for pages. A diagram's exports live on the diagram itself,
            beside the zoom, because SVG and PNG are things you do to the
            *drawing* — what this button saves is the source. */}
        {artifact.kind === "html" && (
          <button type="button" className={styles.ghost} onClick={savePage}>
            {saved === "ok" ? "Saved" : saved === "failed" ? "Couldn’t save" : "Save"}
          </button>
        )}

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
            <Diagram source={artifact.source} name={artifact.title} />
          </div>
        )}
      </div>

      <footer className={styles.foot}>
        <Kbd>Esc</Kbd> closes this
      </footer>
    </aside>
  );
}
