export interface Span {
  start: number;
  end: number;
}

export type Tier = "T1" | "T5";

export enum Trust {
  Deterministic,
  Untrusted,
}

export class Node {
  constructor(readonly span: Span) {}

  isPrecise(): boolean {
    return this.span.end > this.span.start;
  }
}

export const makeSpan = (start: number, end: number): Span => ({ start, end });

export function widen(s: Span): Span {
  return { start: s.start, end: s.end + 1 };
}
