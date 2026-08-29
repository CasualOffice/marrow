/**
 * The Tauri command surface, typed.
 *
 * ─────────────────────────────────────────────────────────────────────────────
 * THESE TYPES ARE HAND-MIRRORED from `crates/desktop/src/commands.rs` until the
 * codegen promised by GUI §2 ("TypeScript types are generated from the Rust
 * command signatures; a drift check fails CI") exists. Every struct there is
 * `#[serde(rename_all = "camelCase")]`, so Rust `relative_path` arrives as
 * `relativePath`. If you change a struct in commands.rs, change it here in the
 * same commit — nothing checks this for you yet.
 * ─────────────────────────────────────────────────────────────────────────────
 *
 * The capability manifest grants the WebView no filesystem, shell or network
 * permission (SEC-012), so the five commands below are the complete surface
 * between this app and the disk.
 */

import { invoke as tauriInvoke } from "@tauri-apps/api/core";

/**
 * Every command goes through here.
 *
 * In a production build this is `tauriInvoke` and nothing else — the dev branch
 * is statically false and Rollup removes it along with the fixtures it imports.
 * In `pnpm dev` opened in a plain browser there is no IPC bridge, so it falls
 * back to `src/dev/fixtures.ts` and the UI can be compared with the mockups.
 */
async function call<T>(
  cmd: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    const { mockInvoke } = await import("./dev/fixtures");
    return mockInvoke<T>(cmd, args);
  }
  return tauriInvoke<T>(cmd, args);
}

/* ── Errors ──────────────────────────────────────────────────────────────── */

/**
 * Mirrors `commands::UiError`.
 *
 * Branch on `code`, never on `message`: a cloud-only file needs a different
 * affordance from a parse failure, and prose is not a contract. `message`
 * already names a cause and an action (UX principle 4) — render it verbatim.
 */
export interface UiError {
  readonly code: string;
  readonly message: string;
}

/** Stable codes this UI gives a distinct affordance to. Others render generically. */
export const Code = {
  /** The file is cloud-only; reading it would trigger a download (invariant #5). */
  PlaceholderSkipped: "FS_PLACEHOLDER_SKIPPED",
  /** Not indexed. The fix is to add a workspace, not to retry. */
  NotFound: "FS_NOT_FOUND",
  /** Permission denied by the OS. */
  Denied: "FS_DENIED",
  /** An internal invariant broke; the message is the only useful thing. */
  Internal: "INT_INVARIANT_VIOLATED",
} as const;

export function isUiError(e: unknown): e is UiError {
  return (
    typeof e === "object" &&
    e !== null &&
    typeof (e as UiError).code === "string" &&
    typeof (e as UiError).message === "string"
  );
}

/** Never invent prose for a failure the core did not describe. */
export function asUiError(e: unknown): UiError {
  if (isUiError(e)) return e;
  return {
    code: "UI_UNEXPECTED",
    message:
      e instanceof Error
        ? e.message
        : "The desktop shell returned something this window could not read.",
  };
}

/* ── search ──────────────────────────────────────────────────────────────── */

/** Mirrors `commands::SearchHit`. */
export interface SearchHit {
  readonly rank: number;
  /** Absolute. Used to call `file_detail` / `read_region`; never displayed. */
  readonly path: string;
  /** Workspace-relative. This is what the row shows. */
  readonly relativePath: string;
  /** `relativePath:line` — the form an editor linkifies. The citation. */
  readonly location: string;
  readonly line: number | null;
  /** The chunker's ancestor chain. Dimmed, last line of the row. */
  readonly breadcrumb: string;
  readonly excerpt: string;
  /** `exact` | `degraded` | `approximate` | `metadata_only`. */
  readonly provenance: string;
  /** Why it matched: `exact` | `semantic` | `path` | `recent`. */
  readonly reason: string;
  /** Invariant #13: `false` means an agent wrote it and it cannot be cited. */
  readonly citable: boolean;
  readonly modifiedMs: number;
  readonly fileId: string;
}

/** Mirrors `commands::SearchResponse`. */
export interface SearchResponse {
  readonly query: string;
  /** Hits on this page. Saturates at the limit — do not show this as a count. */
  readonly total: number;
  /**
   * Documents that matched, before the limit.
   *
   * The footer must use this: "20 results" when the corpus holds 900 is a lie
   * the user has no way to detect.
   */
  readonly matched: number;
  readonly elapsedMs: number;
  readonly hits: readonly SearchHit[];
  /** Retrieval branches that ran. Shown in the footer (UX principle 10). */
  readonly branches: readonly string[];
}

export function search(query: string, limit: number): Promise<SearchResponse> {
  return call<SearchResponse>("search", { query, limit });
}

/* ── list_workspaces ─────────────────────────────────────────────────────── */

/** Mirrors `commands::WorkspaceRow`. */
export interface WorkspaceRow {
  readonly name: string;
  readonly path: string;
  readonly files: number;
}

export function listWorkspaces(): Promise<WorkspaceRow[]> {
  return call<WorkspaceRow[]>("list_workspaces");
}

/* ── index_health ────────────────────────────────────────────────────────── */

/** Mirrors `commands::IndexHealth`. */
export interface IndexHealth {
  readonly files: number;
  readonly chunks: number;
  readonly contentBytes: number;
  /** Never omitted, even at zero (TIER-008). Zero and unknown must differ. */
  readonly cloudOnly: number;
  readonly schemaVersion: number;
}

export function indexHealth(): Promise<IndexHealth> {
  return call<IndexHealth>("index_health");
}

/* ── file_detail ─────────────────────────────────────────────────────────── */

/** Mirrors `commands::FileDetail`. */
export interface FileDetail {
  readonly path: string;
  readonly fileId: string;
  readonly workspace: string;
  readonly sizeBytes: number | null;
  readonly contentHash: string | null;
  readonly mime: string | null;
  readonly modifiedMs: number | null;
  readonly versions: number;
  readonly chunks: number;
  readonly tierState: string;
  readonly citable: boolean;
  readonly previousPaths: readonly string[];
  /**
   * Explicitly `null`, not absent: M1 extracts neither, and absence must stay
   * distinguishable from ignorance (FI-003). Both render as `—`.
   */
  readonly embeddedMetadata: unknown | null;
  readonly structure: unknown | null;
}

export function fileDetail(path: string): Promise<FileDetail> {
  return call<FileDetail>("file_detail", { path });
}

/* ── read_region ─────────────────────────────────────────────────────────── */

/**
 * Lines around a match, and where in the file they start.
 *
 * Bounded on both sides by the core: a 50 MB file returns its matched region,
 * never the whole file (GUI §7). `firstLine` is returned rather than inferred —
 * the UI used to duplicate the core's private context constant to guess it, and
 * the two would drift the first time either changed.
 */
export interface Region {
  /** 1-based line number of `lines[0]`. */
  firstLine: number;
  lines: string[];
  /** Cut short by the cap rather than by the file ending. */
  truncated: boolean;
}

export function readRegion(
  path: string,
  aroundLine?: number,
): Promise<Region> {
  return call<Region>(
    "read_region",
    aroundLine === undefined ? { path } : { path, aroundLine },
  );
}

/** Open a file in whatever the system uses for it. Indexed files only. */
export function openPath(path: string): Promise<void> {
  return call<void>("open_path", { path });
}

/** Reveal a file in the system file manager. Indexed files only. */
export function revealPath(path: string): Promise<void> {
  return call<void>("reveal_path", { path });
}

