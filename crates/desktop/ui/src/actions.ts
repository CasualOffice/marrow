/**
 * The verbs a result supports, in one place, so the keyboard path and the mouse
 * path are literally the same function (GUI §11: "Every mouse action has a
 * keyboard equivalent" — enforced by there being nothing else to call).
 */

import { asUiError, openPath, revealPath } from "./api";
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
 * Hand the file to whatever the system opens it with (`⌘↵`).
 *
 * The core guards this by the index: only an indexed file can be opened, and
 * the path goes to `open` as a single argv element rather than through a shell
 * (SEC-011). A failure is the core's message, verbatim — it names a cause, and
 * "could not open" invented here would name nothing.
 */
export async function openInSystem(path: string, label: string): Promise<void> {
  const { notify } = useUi.getState();
  try {
    await openPath(path);
    notify(`Opened ${label}`);
  } catch (e) {
    notify(asUiError(e).message);
  }
}

/** Show the file in the system file manager (`⇧↵`). */
export async function revealInFileManager(
  path: string,
  label: string,
): Promise<void> {
  const { notify } = useUi.getState();
  try {
    await revealPath(path);
    notify(`Revealed ${label}`);
  } catch (e) {
    notify(asUiError(e).message);
  }
}

/**
 * Things the desktop shell still cannot do.
 *
 * `open_path` and `reveal_path` used to be on this list and are not any more —
 * they exist, they are wired to `⌘↵` and `⇧↵`, and the notices that used to
 * apologise for them are gone. What is left is genuinely absent from
 * `commands.rs`, which exposes eight read-only commands and no mutation at all.
 *
 * These are still bound to their controls rather than hidden, because a
 * shortcut that silently does nothing is worse than one that says why, and each
 * one names the command that would have to exist.
 */
export const MISSING: Record<string, string> = {
  editor:
    'Opening at a line in $EDITOR needs a desktop command that does not exist yet ("open_in_editor"). ⌘↵ opens the file in the system default instead.',
  hydrate:
    'Downloading cloud-only files needs a desktop command that does not exist yet ("workspace_hydrate").',
  policy:
    'Changing what a workspace indexes needs a desktop command that does not exist yet ("workspace_set_policy").',
  retry:
    'Retrying failed parses needs a desktop command that does not exist yet ("job_retry").',
  reindex:
    'Starting an index run needs a desktop command that does not exist yet ("index_run").',
};

export function unavailable(what: keyof typeof MISSING | string): void {
  useUi
    .getState()
    .notify(MISSING[what] ?? "That action has no desktop command yet.");
}
