import { useEffect, useState } from "react";

/**
 * `true` only once `active` has been true for `ms` without interruption.
 *
 * **SKEL-002: a skeleton that flashes is worse than none.** Most searches here
 * return in tens of milliseconds, and a placeholder that appears and vanishes
 * inside one blink reads as a glitch rather than as progress — the eye catches
 * the motion and not the meaning. Holding it back until the wait is long enough
 * to notice means the fast path stays perfectly still.
 *
 * Going false is immediate and deliberate: the delay exists to avoid showing
 * something too eagerly, never to keep it on screen after the answer arrived.
 */
export function useDelayed(active: boolean, ms = 120): boolean {
  const [shown, setShown] = useState(false);

  useEffect(() => {
    if (!active) {
      setShown(false);
      return;
    }
    const t = setTimeout(() => setShown(true), ms);
    return () => clearTimeout(t);
  }, [active, ms]);

  return shown;
}
