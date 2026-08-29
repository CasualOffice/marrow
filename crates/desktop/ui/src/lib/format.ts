/** Formatting rules from UX §4/§5. Nothing here guesses. */

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;
const WEEK = 7 * DAY;
const YEAR = 365 * DAY;

/**
 * Age, not a date. "Recency is what you're judging; `2026-06-14` makes you do
 * arithmetic" (UX §4).
 */
export function age(ms: number, now: number = Date.now()): string {
  if (!Number.isFinite(ms) || ms <= 0) return DASH;
  const d = now - ms;
  if (d < MINUTE) return "now";
  if (d < HOUR) return `${Math.floor(d / MINUTE)}m`;
  if (d < DAY) return `${Math.floor(d / HOUR)}h`;
  if (d < WEEK) return `${Math.floor(d / DAY)}d`;
  if (d < YEAR) return `${Math.floor(d / WEEK)}w`;
  return `${Math.floor(d / YEAR)}y`;
}

/** The long form, for the detail pane where there is room for words. */
export function ageLong(ms: number | null, now: number = Date.now()): string {
  if (ms === null || !Number.isFinite(ms) || ms <= 0) return DASH;
  const a = age(ms, now);
  if (a === "now") return "just now";
  return `${a} ago`;
}

/**
 * UX principle 5: absent metadata renders as this, never as blank. An empty
 * cell reads as "fine"; `—` reads as "we looked, nothing there".
 */
export const DASH = "—";

export function count(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return DASH;
  return n.toLocaleString("en-US");
}

const UNITS = ["B", "KB", "MB", "GB", "TB"] as const;

export function bytes(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return DASH;
  if (n < 1000) return `${n} B`;
  let v = n;
  let u = 0;
  while (v >= 1000 && u < UNITS.length - 1) {
    v /= 1000;
    u += 1;
  }
  return `${v < 10 ? v.toFixed(1) : Math.round(v)} ${UNITS[u] ?? "B"}`;
}

/** Milliseconds, honest about sub-millisecond work rather than rounding to 0. */
export function ms(n: number): string {
  return n < 1 ? "<1 ms" : `${count(n)} ms`;
}

/** `dir/` of a workspace-relative path, or "" at the root. */
export function dirOf(relativePath: string): string {
  const i = relativePath.lastIndexOf("/");
  return i === -1 ? "" : relativePath.slice(0, i + 1);
}

/**
 * Shorten a directory prefix without letting CSS chop it mid-word.
 *
 * `text-overflow: ellipsis` truncates the *end*, which on a path removes the
 * segment that identifies it and leaves `services/vault/src/a…` — the least
 * informative half. Collapsing the middle keeps the root and the immediate
 * parent, which are the two segments that distinguish one result from another.
 */
export function squeezeDir(dir: string, maxChars = 22): string {
  if (dir.length <= maxChars) return dir;
  const parts = dir.split("/").filter(Boolean);
  if (parts.length <= 2) return dir;
  const first = parts[0] as string;
  const last = parts[parts.length - 1] as string;
  const collapsed = `${first}/…/${last}/`;
  return collapsed.length < dir.length ? collapsed : `…/${last}/`;
}

export function baseOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i === -1 ? path : path.slice(i + 1);
}

/** The file extension without the dot, or `—`. */
export function extOf(path: string): string {
  const base = baseOf(path);
  const i = base.lastIndexOf(".");
  return i <= 0 ? DASH : base.slice(i + 1);
}

/** Shorten an absolute path for display. Never used for a command argument. */
export function tilde(abs: string, home = "/Users"): string {
  if (!abs.startsWith(home)) return abs;
  const rest = abs.slice(home.length).split("/").filter(Boolean);
  return rest.length > 1 ? `~/${rest.slice(1).join("/")}` : abs;
}
