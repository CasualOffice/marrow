export function decode(bytes) {
  return new TextDecoder().decode(bytes);
}

export class Chain {
  constructor(parsers) {
    this.parsers = parsers;
  }

  run(input) {
    return this.parsers.find((p) => p.handles(input));
  }
}

export const identity = (x) => x;

const NOT_A_SYMBOL = 42;
