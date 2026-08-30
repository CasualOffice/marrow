/**
 * Markdown for model output.
 *
 * Two rules the rest of the file follows from:
 *
 * 1. **The model's output is untrusted.** It is a language model's guess about
 *    documents that may themselves contain hostile text, so its HTML is
 *    sanitised before it reaches the DOM — the same posture the envelope takes
 *    on the way in.
 * 2. **Fenced blocks that are not prose are extracted, not rendered as code.**
 *    A `mermaid` fence becomes a diagram and an `html` fence becomes a
 *    sandboxed preview; both are handled by the caller, because both need a
 *    React component rather than a string.
 */

import DOMPurify, { type Config } from "dompurify";
import { marked } from "marked";

/** One piece of an answer. */
export type Block =
  | { readonly kind: "markdown"; readonly html: string }
  | { readonly kind: "mermaid"; readonly source: string }
  | { readonly kind: "html"; readonly source: string };

/**
 * Attributes we allow through. Deliberately short: an answer is prose, a list,
 * a table, a link and a code block. Anything else is either decoration or a
 * way in.
 */
const PURIFY: Config = {
  ALLOWED_TAGS: [
    "p", "br", "hr", "strong", "em", "del", "code", "pre", "blockquote",
    "ul", "ol", "li", "h1", "h2", "h3", "h4", "h5", "h6",
    "table", "thead", "tbody", "tr", "th", "td", "a", "span",
  ],
  ALLOWED_ATTR: ["href", "title", "class"],
  // No `data:` and no `javascript:`; a link in an answer points at a document
  // or at the web, never at a script.
  ALLOWED_URI_REGEXP: /^(?:https?:|mailto:|marrow:|#)/i,
  FORBID_TAGS: ["style", "script", "iframe", "object", "embed", "form", "input"],
  FORBID_ATTR: ["style", "onerror", "onload", "onclick"],
};

/**
 * Split an answer into renderable blocks.
 *
 * Streaming-safe: an unterminated fence at the end of the buffer is treated as
 * still-arriving and rendered as its own block, so a diagram does not flicker
 * into existence as a wall of code first.
 */
export function parseAnswer(markdown: string): Block[] {
  const blocks: Block[] = [];
  // A hand-rolled scan rather than a `marked` extension: we need the
  // *unclosed* trailing fence too, and a tokenizer will not hand that over.
  const lines = markdown.split("\n");
  let i = 0;
  let prose: string[] = [];

  const flushProse = () => {
    const text = prose.join("\n").trim();
    prose = [];
    if (text) blocks.push({ kind: "markdown", html: renderMarkdown(text) });
  };

  while (i < lines.length) {
    const line = lines[i] ?? "";
    const open = /^```([a-zA-Z0-9_-]*)\s*$/.exec(line);
    const lang = open?.[1]?.toLowerCase();
    if (open && (lang === "mermaid" || lang === "html")) {
      flushProse();
      const body: string[] = [];
      i += 1;
      while (i < lines.length && !/^```\s*$/.test(lines[i] ?? "")) {
        body.push(lines[i] ?? "");
        i += 1;
      }
      i += 1; // the closing fence, or the end of the buffer
      blocks.push(
        lang === "mermaid"
          ? { kind: "mermaid", source: body.join("\n") }
          : { kind: "html", source: body.join("\n") },
      );
      continue;
    }
    prose.push(line);
    i += 1;
  }
  flushProse();
  return blocks;
}

/** Markdown to sanitised HTML. */
export function renderMarkdown(md: string): string {
  const raw = marked.parse(md, { async: false, gfm: true, breaks: false });
  return DOMPurify.sanitize(raw as string, PURIFY) as unknown as string;
}

/**
 * Turn `[E1]` into a clickable citation.
 *
 * Done on the sanitised HTML rather than on the Markdown, so a citation inside
 * a code fence is left alone — a model writing `[E1]` in an example is not
 * making a claim.
 */
export function linkCitations(html: string, known: ReadonlySet<string>): string {
  return html.replace(/\[(E\d+)\]/g, (whole, id: string) =>
    known.has(id)
      ? `<a href="marrow:cite/${id}" class="cite" data-cite="${id}">[${id}]</a>`
      : whole,
  );
}
