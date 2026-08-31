/**
 * The verbs a result supports, in one place, so the keyboard path and the mouse
 * path are literally the same function (GUI §11: "Every mouse action has a
 * keyboard equivalent" — enforced by there being nothing else to call).
 */

import {
  addFiles,
  addWorkspace,
  asUiError,
  clearScratch,
  openPath,
  reindex,
  revealPath,
  type DropReport,
} from "./api";
import { SCRATCH_KEY } from "./queries";
import { queryClient } from "./queryClient";
import type { Anchor } from "./store";
import { useUi } from "./store";

/**
 * Everything that changes when the set of indexed files changes.
 *
 * One list, called by every verb that adds or removes files, because a dropped
 * file is simultaneously a new workspace, a new file row, a different count on
 * Status, a different ranking and possibly a new project to scope to. Refetching
 * only the panel that happens to be on screen leaves the other four showing the
 * state from before — and the first-run flow reads two of them to decide
 * whether its step 1 is done, so a stale answer there is the flow refusing to
 * acknowledge what the user just did.
 *
 * `["files"]` and `["search"]` are key *prefixes*: those queries carry a
 * workspace, a filter and a limit, and every variant of them is equally stale.
 */
export function refreshAfterIndexChange(): void {
  for (const queryKey of [
    ["workspaces"],
    ["health"],
    ["files"],
    ["search"],
    ["projects"],
    SCRATCH_KEY,
  ]) {
    void queryClient.invalidateQueries({ queryKey });
  }
}

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
 * Sweep every granted folder against the disk (`reindex`).
 *
 * Here rather than in the view because two controls need it — the empty
 * workspace and the freshness banner — and a button that reports one thing in
 * one place and something else in the other is two bugs waiting.
 *
 * It resolves when the folders have been *asked*, not when they are done: a
 * pass over 78,000 files takes minutes. So the notice says what was started,
 * never that anything finished.
 */
/**
 * Grant Marrow a folder, from wherever the user is standing.
 *
 * Shared so the Status page and the zero-results page cannot drift: the second
 * one spent its life raising a notice about `workspace_set_policy` — a
 * different feature — on a button labelled "Add a folder", long after the
 * command it needed existed.
 */
export async function grantFolder(): Promise<void> {
  const { notify } = useUi.getState();
  try {
    const next = await addWorkspace();
    if (next) {
      // The list this returned is the truth as of now; writing it in beats
      // waiting up to thirty seconds for the poll. The rest of the panels are
      // invalidated rather than written, because this call does not know what
      // they should say.
      queryClient.setQueryData(["workspaces"], next);
      refreshAfterIndexChange();
      notify("Indexing it now. It becomes searchable as it goes.");
    }
  } catch (e) {
    notify(asUiError(e).message);
  }
}

/**
 * What a drop did, in one sentence.
 *
 * Shared by the drop event, the file picker and the setup flow, because those
 * three produce the same outcome and a user who drops a file should not read a
 * different account of it than one who picked the same file from a panel.
 *
 * Every part of the report gets a clause, including the ones that are not
 * successes: a file that was skipped, or an older copy that was evicted to make
 * room, is exactly what the user needs to hear about and exactly what a
 * summary is tempted to drop.
 */
export function describeDrop(r: DropReport): string {
  const parts: string[] = [];
  if (r.added.length === 1) parts.push(`Added ${r.added[0]}`);
  else if (r.added.length > 1) parts.push(`Added ${r.added.length} files`);
  if (r.alreadyThere.length > 0) {
    parts.push(
      r.alreadyThere.length === 1
        ? `${r.alreadyThere[0]} was already there`
        : `${r.alreadyThere.length} were already there`,
    );
  }
  if (r.evicted.length > 0) {
    parts.push(
      `made room by removing ${r.evicted.length === 1 ? r.evicted[0] : `${r.evicted.length} older files`}`,
    );
  }
  // The first refusal in full, because one clear reason is worth more than a
  // count of reasons the user cannot see.
  const first = r.skipped[0];
  if (first) {
    parts.push(
      r.skipped.length === 1
        ? `Skipped ${first.name} — ${first.reason}`
        : `Skipped ${r.skipped.length} files. ${first.name}: ${first.reason}`,
    );
  }
  if (parts.length === 0) return "Nothing was added.";
  return `${parts.join(". ")}.`;
}

/**
 * Copy files into the dropped-files workspace (`⌘O`).
 *
 * The keyboard half of dropping onto the window: a drop is a mouse gesture with
 * no keyboard equivalent at all, and GUI §11 does not have an exception for
 * gestures the OS invented.
 */
export async function pickFiles(): Promise<DropReport | null> {
  const { notify } = useUi.getState();
  try {
    const report = await addFiles();
    if (report) {
      notify(describeDrop(report));
      refreshAfterIndexChange();
    }
    return report;
  } catch (e) {
    notify(asUiError(e).message);
    return null;
  }
}

/**
 * Throw away everything in the dropped-files workspace.
 *
 * Deletes copies Marrow made in a folder Marrow owns; the user's own files are
 * never touched. Deliberately not confirmed with a dialog: what it removes is
 * duplicated bytes, and the originals are wherever they came from.
 */
export async function emptyScratch(): Promise<void> {
  const { notify } = useUi.getState();
  try {
    const r = await clearScratch();
    refreshAfterIndexChange();
    notify(
      r.removed.length === 0
        ? "There was nothing in the dropped-files folder."
        : `Removed ${r.removed.length} ${r.removed.length === 1 ? "file" : "files"}. They are no longer searchable.`,
    );
  } catch (e) {
    notify(asUiError(e).message);
  }
}

export async function runIndex(): Promise<void> {
  const { notify } = useUi.getState();
  try {
    const n = await reindex();
    notify(
      n === 1
        ? "Checking your folder against the disk. The counts update as it goes."
        : `Checking ${n} folders against the disk. The counts update as they go.`,
    );
  } catch (e) {
    notify(asUiError(e).message);
  }
}

/**
 * Things the desktop shell still cannot do.
 *
 * `open_path`, `reveal_path` and `reindex` used to be on this list and are not
 * any more — they exist, they are wired to `⌘↵`, `⇧↵` and "Run an index", and
 * the notices that used to apologise for them are gone. What is left is
 * genuinely absent from `commands.rs`.
 *
 * These are still bound to their controls rather than hidden, because a
 * shortcut that silently does nothing is worse than one that says why, and each
 * one names the command that would have to exist.
 */
export const MISSING: Record<string, string> = {
  editor:
    'Opening at a line in $EDITOR needs a desktop command that does not exist yet ("open_in_editor"). ⌘↵ opens the file in the system default instead.',
  // `hydrate`, `policy` and `retry` were here and are gone. Each described a
  // command that "does not exist yet", and none of them was waiting on one:
  // hydrating a cloud placeholder is refused by hard rule 3, "keep as is" was
  // a button for the thing already happening, and re-running a parse over
  // unchanged bytes with the same parser produces the same nothing — what
  // re-reads a file is the file changing or a better parser shipping. A
  // promise of a future command is a worse answer than the reason.
};

export function unavailable(what: keyof typeof MISSING | string): void {
  useUi
    .getState()
    .notify(MISSING[what] ?? "That action has no desktop command yet.");
}
