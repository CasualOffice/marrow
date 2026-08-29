/**
 * Where the query appears in a piece of text.
 *
 * The core strips FTS5's match delimiters before the excerpt reaches this
 * window, so the UI is given text with no offsets in it. It can still say
 * *which* words the user asked for, and marking them is what turns "this row
 * looks unrelated" into "the term is right there" — the complaint that started
 * as "its confusing".
 *
 * Deliberately dumb: literal, case-insensitive, whole-substring. It marks what
 * the user typed and nothing else. It does not stem, expand or guess, because a
 * highlight on a word the user did not type is a claim about the ranking that
 * this layer is not entitled to make — and a wrong highlight is worse than
 * none.
 */

export interface Segment {
  readonly text: string;
  readonly hit: boolean;
}

/** The distinct terms in a query, longest first so overlaps prefer the longer. */
export function terms(query: string): string[] {
  const seen = new Set<string>();
  for (const w of query.toLowerCase().split(/[^\p{L}\p{N}_]+/u)) {
    if (w.length >= 2) seen.add(w);
  }
  return [...seen].sort((a, b) => b.length - a.length);
}

/**
 * Split `text` into runs, marking the ones that are a query term.
 *
 * Returns a single unmarked segment when there is nothing to mark, so a caller
 * can render the result unconditionally.
 */
export function segments(text: string, ts: readonly string[]): Segment[] {
  if (ts.length === 0 || text === "") return [{ text, hit: false }];

  const lower = text.toLowerCase();
  // One pass, marking every character covered by any term.
  const marked = new Uint8Array(text.length);
  let any = false;
  for (const t of ts) {
    let from = 0;
    for (;;) {
      const i = lower.indexOf(t, from);
      if (i === -1) break;
      marked.fill(1, i, i + t.length);
      any = true;
      from = i + t.length;
    }
  }
  if (!any) return [{ text, hit: false }];

  const out: Segment[] = [];
  let start = 0;
  for (let i = 1; i <= text.length; i += 1) {
    if (i === text.length || marked[i] !== marked[start]) {
      out.push({ text: text.slice(start, i), hit: marked[start] === 1 });
      start = i;
    }
  }
  return out;
}

/** True when any term occurs in the text. Cheaper than building segments. */
export function contains(text: string, ts: readonly string[]): boolean {
  if (ts.length === 0) return false;
  const lower = text.toLowerCase();
  return ts.some((t) => lower.includes(t));
}
