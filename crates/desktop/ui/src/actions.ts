/**
 * The verbs a result supports, in one place, so the keyboard path and the mouse
 * path are literally the same function (GUI §11: "Every mouse action has a
 * keyboard equivalent" — enforced by there being nothing else to call).
 */

import type { Anchor } from "./store";
import { useUi } from "./store";

/**
 * Copy the citation — `relativePath:line`, the form an editor linkifies.
 *
 * `navigator.clipboard` is the only clipboard this window has: the capability
 * manifest grants no plugins, and `clipboard-manager` is not among them. The
 * `execCommand` path is the fallback for a WebView that refuses the async API
 * without a user-gesture heuristic we can't influence.
 */
export async function copyCitation(anchor: Anchor): Promise<void> {
  const text = anchor.location;
  const { notify } = useUi.getState();
  try {
    await navigator.clipboard.writeText(text);
    notify(`Copied ${text}`);
    return;
  } catch {
    /* fall through */
  }
  try {
    const el = document.createElement("textarea");
    el.value = text;
    el.setAttribute("readonly", "");
    el.style.position = "fixed";
    el.style.opacity = "0";
    document.body.appendChild(el);
    el.select();
    const ok = document.execCommand("copy");
    document.body.removeChild(el);
    notify(ok ? `Copied ${text}` : "The clipboard refused the copy.");
  } catch {
    notify("The clipboard refused the copy.");
  }
}

/**
 * Things the desktop shell cannot do yet.
 *
 * `commands.rs` exposes five read-only commands and a test asserts the surface
 * stays that way in M1, so "open in $EDITOR", "reveal in Finder", "index these
 * files" and "include this folder" have no command to call. They are still
 * bound to their keys and still rendered as buttons — a shortcut that silently
 * does nothing is worse than one that says why — and each names the command
 * that has to exist. See the report accompanying this UI.
 */
export const MISSING: Record<string, string> = {
  open:
    'Opening in the default app needs a desktop command that does not exist yet ("open_path").',
  editor:
    'Opening at a line in $EDITOR needs a desktop command that does not exist yet ("open_in_editor").',
  reveal:
    'Reveal in Finder needs a desktop command that does not exist yet ("reveal_in_file_manager").',
  hydrate:
    'Downloading cloud-only files needs a desktop command that does not exist yet ("workspace_hydrate").',
  policy:
    'Changing what a workspace indexes needs a desktop command that does not exist yet ("workspace_set_policy").',
  retry:
    'Retrying failed parses needs a desktop command that does not exist yet ("job_retry").',
};

export function unavailable(what: keyof typeof MISSING | string): void {
  useUi
    .getState()
    .notify(MISSING[what] ?? "That action has no desktop command yet.");
}
