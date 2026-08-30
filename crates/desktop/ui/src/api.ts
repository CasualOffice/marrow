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
 * permission (SEC-012), so the commands below are the complete surface
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
  /**
   * The one workspace Marrow created for itself — where dropped files land.
   *
   * Two things in this window have to treat it differently, and both would
   * otherwise be guessing from the name: the first-run flow asks whether the
   * user has got anywhere yet, and an *empty* scratch folder is "empty" rather
   * than the "nothing indexed" fault an empty granted folder is. Nothing else
   * about it is special, deliberately.
   */
  readonly scratch: boolean;
  readonly files: number;
  /**
   * Per workspace, not global.
   *
   * GUI §11 requires every degraded state to be visible from the sidebar
   * without navigating, and one global number cannot say *which* workspace is
   * the problem. `parseFailed`, `notProcessed` and `cloudOnly` above zero are
   * the degraded states; `noParser` is not one.
   */
  readonly chunks: number;
  readonly contentBytes: number;
  /** Contents deliberately not read. Never omitted, even at zero (TIER-008). */
  readonly cloudOnly: number;
  /**
   * Files with no searchable contents, for any reason. The total.
   *
   * Not a fault count, which is what it was being rendered as: it counts a
   * folder of photos exactly as it counts a folder of corrupt PDFs. The three
   * below say which, and sum to this.
   */
  readonly unindexed: number;
  /**
   * Expected. Nothing to index — a photo, a font, a binary — and still findable
   * by name and date (T5). Shown, never warned about.
   */
  readonly noParser: number;
  /** Wrong, and fixable: the text is on disk and Marrow does not have it. */
  readonly parseFailed: number;
  /** Not reached yet by any ingest run. A sweep clears it. */
  readonly notProcessed: number;
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
  /** When the index last agreed with the disk. `null` means it never has. */
  readonly lastIndexedMs: number | null;
  /** `live` | `degraded` | `poll_only` | `unavailable`, worst root wins. */
  readonly watcher: string;
  /** True when nothing is keeping the index in step with the disk. */
  readonly mayBeStale: boolean;
}

/**
 * Grant Marrow a folder. Opens a native picker; the WebView never sees a path
 * it did not receive back from this call.
 *
 * Resolves `null` when the picker was cancelled — not an error, and not a
 * reason to show one.
 */
export function addWorkspace(): Promise<readonly WorkspaceRow[] | null> {
  return call<readonly WorkspaceRow[] | null>("add_workspace", {});
}

/* ── dropped files ───────────────────────────────────────────────────────── */

/** Mirrors `scratch::Skipped` — one file that did not make it in, and why. */
export interface DropSkipped {
  readonly name: string;
  /** A code from the §108 taxonomy. Branch on this, never on the prose. */
  readonly code: string;
  readonly reason: string;
}

/** Mirrors `scratch::DropReport`. Nothing a drop did is silent. */
export interface DropReport {
  readonly added: readonly string[];
  /** Already present with identical contents. Not a failure — dedup is a
   *  feature — but the user asked for something and must be told what
   *  happened. */
  readonly alreadyThere: readonly string[];
  readonly skipped: readonly DropSkipped[];
  /** Older copies removed to stay under the folder's ceiling. Reported,
   *  because an eviction nobody is told about is a file that silently
   *  vanished from under a saved answer's citations. */
  readonly evicted: readonly string[];
  readonly bytesAdded: number;
  readonly workspace: string;
}

/**
 * Mirrors `commands::DropOutcome` — the payload on the drop event.
 *
 * Exactly one of the two is set. A whole-drop failure (the folder could not be
 * created, the store refused) is a different thing from the per-file refusals
 * inside a report, and reads differently.
 */
export interface DropOutcome {
  readonly report: DropReport | null;
  readonly error: UiError | null;
}

/** Mirrors `scratch::ScratchStatus`. Counted from the disk, not from the index:
 *  the question it answers is what this is costing in bytes. */
export interface ScratchStatus {
  /** False before the first drop — the folder is created on demand. */
  readonly exists: boolean;
  /** `null` until it exists. The window never prints a guessed path. */
  readonly path: string | null;
  readonly workspace: string;
  readonly files: number;
  readonly bytes: number;
  readonly maxBytes: number;
  readonly maxFileBytes: number;
}

/** Mirrors `scratch::ClearReport`. */
export interface ClearReport {
  readonly removed: readonly string[];
  readonly bytes: number;
}

/**
 * Copy chosen files into the scratch workspace and index them.
 *
 * Opens a native panel in Rust and **takes no path argument** — the same shape
 * as `addWorkspace`, and for the same reason. SEC-012 means this window has no
 * filesystem affordance at all, so there is deliberately no way to say "index
 * this path" from here even if the window wanted to.
 *
 * Resolves `null` when the panel was cancelled, which is not an error.
 */
export function addFiles(): Promise<DropReport | null> {
  return call<DropReport | null>("add_files", {});
}

export function scratchStatus(): Promise<ScratchStatus> {
  return call<ScratchStatus>("scratch_status", {});
}

/**
 * Empty the scratch workspace.
 *
 * Deletes copies Marrow made inside a folder Marrow owns; nothing the user
 * wrote is touched. The index rows are soft-deleted by the ordinary ingest
 * path, exactly as they would be for any file that disappeared from a watched
 * folder.
 */
export function clearScratch(): Promise<ClearReport> {
  return call<ClearReport>("clear_scratch", {});
}

/** What a drag over the window is carrying, for the overlay. */
export interface DragState {
  readonly over: boolean;
  readonly count: number;
}

/**
 * Subscribe to the window's drag-and-drop life cycle.
 *
 * Three of the four events are Tauri's own (`tauri://drag-enter` and friends),
 * which is where the hover overlay's file count comes from. **Those paths are
 * used to draw and are never sent back**: no command accepts a source path, so
 * the only paths that reach the disk are the ones the OS handed to Rust. The
 * fourth is Marrow's, carrying what the copy-and-index actually did.
 *
 * Resolves to an unsubscribe. In a plain browser (`pnpm dev`) there is no IPC
 * bridge and nothing to listen to, so it resolves to a no-op rather than
 * throwing — the rest of the window still works there.
 */
export async function onDragAndDrop(handlers: {
  onDrag: (s: DragState) => void;
  /**
   * The files landed and Rust has started copying them.
   *
   * Distinct from `onDrag({over: false})`, and it has to be: a drag pulled back
   * out of the window also ends the hover and produces **no** drop, so a window
   * that treated the two alike would put up a "working…" state that nothing
   * would ever take down.
   */
  onDropStarted: (count: number) => void;
  onDrop: (o: DropOutcome) => void;
}): Promise<() => void> {
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    return () => {};
  }
  const { listen } = await import("@tauri-apps/api/event");
  const off = await Promise.all([
    listen<{ paths?: string[] }>("tauri://drag-enter", (e) =>
      handlers.onDrag({ over: true, count: e.payload?.paths?.length ?? 0 }),
    ),
    // `tauri://drag-over` is deliberately not listened to: it fires
    // continuously while the pointer moves and its payload carries no paths, so
    // subscribing would replace the count from `drag-enter` with a zero on the
    // first mouse move.
    listen("tauri://drag-leave", () => handlers.onDrag({ over: false, count: 0 })),
    listen<{ paths?: string[] }>("tauri://drag-drop", (e) => {
      handlers.onDrag({ over: false, count: 0 });
      handlers.onDropStarted(e.payload?.paths?.length ?? 0);
    }),
    listen<DropOutcome>("marrow://drop-result", (e) => handlers.onDrop(e.payload)),
  ]);
  return () => off.forEach((fn) => fn());
}

/** One folder a question can be narrowed to. */
export interface Project {
  readonly path: string;
  readonly files: number;
}

/**
 * The projects a question can be scoped to.
 *
 * Derived from what is indexed rather than configured, and by the same rule the
 * answer uses when it names which projects it drew from — the two have to agree,
 * or narrowing to a project you were shown would not narrow to that project.
 */
export function listProjects(): Promise<readonly Project[]> {
  return call<readonly Project[]>("list_projects", {});
}

export function indexHealth(): Promise<IndexHealth> {
  return call<IndexHealth>("index_health");
}

/**
 * Ask every watched folder to reconcile with the disk now.
 *
 * Resolves with the number of folders asked, as soon as they have been asked —
 * a full pass takes minutes, and a promise that settled then would look hung.
 * The counts on this page catch up as the sweep stores what it finds.
 */
export function reindex(): Promise<number> {
  return call<number>("reindex");
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

/* ── list_files ──────────────────────────────────────────────────────────── */

/** Mirrors `commands::FileRow`. */
export interface FileRow {
  readonly path: string;
  readonly relativePath: string;
  readonly workspace: string;
  readonly sizeBytes: number | null;
  readonly modifiedMs: number | null;
  readonly chunks: number;
  /**
   * Recorded, but with no searchable contents.
   *
   * The Files view must render this row differently from an indexed one — a
   * file you can find by name but not by what is inside it is a different
   * thing, and a browser that draws them identically is lying by omission.
   */
  readonly metadataOnly: boolean;
}

/**
 * Browse the index, newest first.
 *
 * Browsing is not searching. The Files view used to be built on `search`, so
 * with no query it showed nothing at all for an index holding 35,000 files.
 */
export function listFiles(
  args: {
    workspace?: string | undefined;
    prefix?: string | undefined;
    limit: number;
  },
): Promise<FileRow[]> {
  return call<FileRow[]>("list_files", {
    workspace: args.workspace ?? null,
    prefix: args.prefix ?? null,
    limit: args.limit,
  });
}


// ── models (Part 8) ───────────────────────────────────────────────────────

/** One tier of the tiered design (§139.5): router, generator, embedder. */
export interface RoleRow {
  readonly workload: string;
  readonly paramsB: number;
  /** Whether it stays in memory between requests. */
  readonly resident: boolean;
  readonly why: string;
}

export interface ProfileRow {
  readonly id: string;
  readonly label: string;
  readonly detail: string;
  readonly generatorParamsB: number;
  readonly selected: boolean;
  readonly available: boolean;
  /** With the arithmetic, never a bare greyed-out row (TIER-026). */
  readonly unavailableReason: string | null;
}

export interface DetectedRow {
  readonly runtime: string;
  readonly port: number;
  readonly modelCount: number;
}

/** The supervisor's lifecycle state, tagged (§142.2). */
export type ModelState =
  | { readonly state: "absent" }
  | { readonly state: "installed" }
  | { readonly state: "loading"; readonly stage: string }
  | { readonly state: "ready" }
  | { readonly state: "busy" }
  | { readonly state: "unloading" }
  | { readonly state: "suspended"; readonly reason: string };

/** Where a transfer is, in the words SKEL-006 requires. */
export type DownloadStage =
  | { readonly stage: "downloading"; readonly file: string; readonly index: number; readonly of: number }
  | { readonly stage: "verifying"; readonly file: string }
  | { readonly stage: "ready" }
  | { readonly stage: "cancelled" }
  | { readonly stage: "failed"; readonly code: string; readonly reason: string };

export interface DownloadProgress {
  readonly modelId: string;
  readonly stage: DownloadStage;
  readonly bytesDone: number;
  readonly bytesTotal: number;
  readonly bytesPerSec: number;
  /** `null` until there is a rate worth dividing by (SKEL-005). */
  readonly etaSecs: number | null;
}

export interface ModelRow {
  readonly id: string;
  readonly displayName: string;
  readonly family: string;
  readonly paramsB: number;
  readonly quantization: string;
  readonly format: string;
  readonly contextLimit: number;
  readonly role: string;
  readonly source: "catalogue" | "detected" | "user_supplied";
  readonly detectedIn: string | null;
  readonly installed: boolean;
  readonly downloadable: boolean;
  /**
   * Why there is no download button, phrased for a human. `null` when there is
   * nothing to explain — rendering a button that cannot work would be the
   * Settings bug in a different costume.
   */
  readonly blockedReason: string | null;
  /** Where the weights come from, and the commit they are pinned to. */
  readonly repo: string | null;
  readonly revisionShort: string | null;
  readonly fileCount: number;
  readonly downloadBytes: number;
  /** What it is sized at. `contextLimit` is what it advertises. */
  readonly runContext: number;
  /** True when the KV figure was read from the model's config, not guessed. */
  readonly kvMeasured: boolean;
  readonly progress: DownloadProgress | null;
  readonly licence: string;
  readonly licenceUrl: string | null;
  /** `null` is "not established" — neither yes nor no (LIC-004). */
  readonly commercialUse: boolean | null;
  readonly capabilities: readonly string[];
  /** Why Fast/Thorough is disabled for this model (GEN-013). */
  readonly reasoningUnavailable: string | null;
  readonly fit: "comfortable" | "tight" | "too_large";
  readonly fitReason: string;
  /** weights · KV · runtime · embedder · reserve, already formatted. */
  readonly breakdown: string;
  readonly requiredBytes: number;
  readonly state: ModelState;
  readonly consecutiveFailures: number;
  readonly suspendedReason: string | null;
}

/** Whether semantic search is on, and how far along building it is. */
export interface SemanticStatus {
  /** True once an embedder has actually loaded. */
  readonly ready: boolean;
  readonly embedded: number;
  readonly remaining: number;
  readonly failed: number;
  readonly running: boolean;
  /** Why it is unavailable. `null` when it is fine. */
  readonly problem: string | null;
  readonly model: string | null;
}

export interface ModelsSnapshot {
  readonly machine: string;
  readonly tierHeadline: string;
  readonly unifiedMemory: boolean;
  readonly totalBytes: number;
  /** From the live sampler, not the launch probe (LLM-019). */
  readonly availableBytes: number;
  readonly sustainedLoad: number;
  readonly thermal: string;
  /** True when the sampler has stopped reporting (HW-015). */
  readonly sampleStale: boolean;
  /** How much of the index semantic search actually covers. */
  readonly semantic: SemanticStatus;
  readonly residentBytes: number;
  /** Why the model directory is unusable, if it is (SUP-011). */
  readonly modelsDirProblem: string | null;
  readonly detected: readonly DetectedRow[];
  readonly detectionProblems: readonly string[];
  readonly profiles: readonly ProfileRow[];
  readonly router: RoleRow;
  readonly generator: RoleRow;
  readonly embedder: RoleRow;
  readonly models: readonly ModelRow[];
  readonly runtimeStatus: string;
  /** False means every model here can be downloaded and none can answer. */
  readonly runtimeReady: boolean;
  /** The commands that would create a runtime. Named, because "MLX is not
   *  available" is a dead end and this is something the user can do. */
  readonly runtimeSetup: string | null;
  /** The remote endpoint, if one is configured. `runtimeStatus` is written
   *  from it: "nothing leaves this device" is only true while it is off. */
  readonly remote: ProviderStatus;
}

/**
 * Mirrors `models::ProviderStatus` — the configured endpoint, and what would
 * stop it being used.
 *
 * **There is no key field, on purpose.** Whether one is stored is something
 * this window needs to know; what it is, is not, and no command returns it
 * (LLM-030).
 */
export interface ProviderStatus {
  readonly configured: boolean;
  readonly enabled: boolean;
  readonly label: string;
  readonly baseUrl: string;
  readonly model: string;
  readonly maxOutputTokens: number;
  readonly reasoningEffort: string | null;
  /** `local` · `private` · `cloud`, decided by the address the endpoint
   *  resolves to rather than by what was typed. */
  readonly boundary: string | null;
  readonly boundaryLabel: string | null;
  /** The addresses the connection would be pinned to. A boundary the user
   *  cannot check is a claim rather than a fact. */
  readonly addresses: readonly string[];
  readonly hasKey: boolean;
  readonly problem: string | null;
  /** A workspace classification that forbids it outright (MOD-004). */
  readonly blockedBy: string | null;
}

/** What the provider form sends. `key` is write-only and never comes back. */
export interface ProviderForm {
  readonly enabled: boolean;
  readonly label: string;
  readonly baseUrl: string;
  readonly model: string;
  readonly maxOutputTokens: number;
  readonly reasoningEffort: string | null;
  /** `null` leaves whatever is in the keychain alone. */
  readonly key: string | null;
}

export function providerSettings(): Promise<ProviderStatus> {
  return call<ProviderStatus>("provider_settings", {});
}

export function setCloudProvider(form: ProviderForm): Promise<ProviderStatus> {
  return call<ProviderStatus>("set_cloud_provider", { form });
}

export function clearCloudProvider(): Promise<ProviderStatus> {
  return call<ProviderStatus>("clear_cloud_provider", {});
}

/**
 * Build semantic search over everything already indexed.
 *
 * Deliberately a button rather than something that happens on its own: it
 * loads a model and runs for minutes. Keyword search never needs it — that is
 * the half that works with no model, no GPU and no network.
 */
export function startSemanticBackfill(): Promise<ModelsSnapshot> {
  return call<ModelsSnapshot>("start_semantic_backfill", {});
}

export function stopSemanticBackfill(): Promise<ModelsSnapshot> {
  return call<ModelsSnapshot>("stop_semantic_backfill", {});
}

export function modelsOverview(): Promise<ModelsSnapshot> {
  return call<ModelsSnapshot>("models_overview", {});
}

export function refreshModelDetection(): Promise<ModelsSnapshot> {
  return call<ModelsSnapshot>("refresh_model_detection", {});
}

export function setAiProfile(profile: string): Promise<ModelsSnapshot> {
  return call<ModelsSnapshot>("set_ai_profile", { profile });
}

export function downloadModel(modelId: string): Promise<ModelsSnapshot> {
  return call<ModelsSnapshot>("download_model", { modelId });
}

export function cancelModelDownload(modelId: string): Promise<ModelsSnapshot> {
  return call<ModelsSnapshot>("cancel_model_download", { modelId });
}

export function dismissModelDownload(modelId: string): Promise<ModelsSnapshot> {
  return call<ModelsSnapshot>("dismiss_model_download", { modelId });
}

// ── ask (Part 8 §148) ─────────────────────────────────────────────────────

export interface Citation {
  readonly id: string;
  readonly path: string;
  readonly relativePath: string;
  readonly location: string;
  readonly line: number | null;
  readonly excerpt: string;
  readonly provenance: string;
}

export interface ExcludedSource {
  readonly relativePath: string;
  readonly reason: string;
}

export type AskEvent =
  /**
   * The handle `cancelAsk` needs, sent before anything else happens.
   *
   * The `ask` promise also resolves with it — but only when the answer is
   * *finished*, which is exactly too late: for the whole time the Stop button
   * was on screen there was nothing for it to cancel, and both it and Esc did
   * nothing.
   */
  | { readonly kind: "started"; readonly id: string }
  /** What the pipeline is doing right now (SKEL-003, SKEL-006). */
  | { readonly kind: "stage"; readonly stage: string; readonly detail: string }
  | {
      readonly kind: "sources";
      readonly hits: readonly Citation[];
      readonly excluded: readonly ExcludedSource[];
      /** UX-013: what left the device, even when the answer is local. */
      readonly bytes: number;
      readonly distinctSources: number;
      /**
       * The distinct projects the evidence came from, relative to the
       * workspace root — `services/STT`, `services/vault`. More than one means
       * the answer was stitched across unrelated bodies of work and the reader
       * has to be told; one granted folder is routinely a dozen projects.
       *
       * Optional only because the dev fixtures do not send it yet. The Rust
       * event always does, so treat a missing value as "not known" rather than
       * as "one project".
       */
      readonly projects?: readonly string[];
      /** UX-012: stated for every generation. `local` · `private` · `cloud`. */
      readonly boundary: string;
      /** The same fact in the words to show. Sent rather than mapped here, so
       *  there is one set of words and Rust owns it. */
      readonly boundaryLabel: string;
      /** The host the excerpts go to, or `null` when they go nowhere. */
      readonly destination: string | null;
      readonly model: string;
    }
  | { readonly kind: "token"; readonly text: string }
  | { readonly kind: "thinking"; readonly text: string }
  | {
      readonly kind: "done";
      readonly promptTokens: number;
      readonly outputTokens: number;
      readonly thinkingTokens: number;
      readonly cachedPrefixTokens: number;
      readonly stopReason: string;
      readonly elapsedMs: number;
    }
  | { readonly kind: "failed"; readonly code: string; readonly message: string };

/**
 * Ask, streaming.
 *
 * A `Channel` rather than a promise of a string: an answer that arrives all at
 * once is a spinner with extra steps, and SKEL-004 requires content to replace
 * the skeleton as it comes.
 */
export interface PriorTurn {
  readonly role: "user" | "assistant";
  readonly text: string;
}

export async function ask(
  args: {
    conversation: string;
    question: string;
    history: readonly PriorTurn[];
    thorough: boolean;
    /**
     * A subtree the answer is confined to, relative to the workspace root —
     * `services/STT`. Omitted asks the whole index, which is the right default
     * and the wrong answer when one granted folder holds many unrelated
     * projects: "what is STT?" was answered from the STT service, an MFA
     * setting and a code of conduct at once.
     */
    /** Restrict retrieval to one project. `null` is every project. */
    scope?: string | null;
  },
  onEvent: (e: AskEvent) => void,
): Promise<string> {
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    const { mockAsk } = await import("./dev/fixtures");
    return mockAsk(args.question, args.thorough, onEvent, args.history.length);
  }
  const { Channel, invoke } = await import("@tauri-apps/api/core");
  const channel = new Channel<AskEvent>();
  channel.onmessage = onEvent;
  return invoke<string>("ask", {
    conversation: args.conversation,
    question: args.question,
    history: args.history,
    thorough: args.thorough,
    // Explicitly null rather than absent, so `Option<String>` has a value to
    // read whatever the command deserializer would make of a missing field.
    scope: args.scope ?? null,
    onEvent: channel,
  });
}

export function forgetConversation(conversation: string): Promise<void> {
  return call<void>("forget_conversation", { conversation });
}

/* ── conversations ───────────────────────────────────────────────────────── */

/** Mirrors `commands::ConversationSummary`. */
export interface ConversationSummary {
  readonly id: string;
  readonly title: string;
  /** The project it was last scoped to. `null` is every project. */
  readonly scope: string | null;
  readonly createdMs: number;
  /**
   * When it was last *used*, which is what the list is ordered by — a thread
   * you came back to this morning belongs above one abandoned last week.
   */
  readonly updatedMs: number;
  readonly turns: number;
}

/** Mirrors `commands::TurnUsage`, which mirrors the `done` event. */
export interface TurnUsage {
  readonly promptTokens: number;
  readonly outputTokens: number;
  readonly thinkingTokens: number;
  readonly cachedPrefixTokens: number;
  readonly stopReason: string;
  readonly elapsedMs: number;
  /**
   * Where the answer was generated, and the words shown at the time (UX-012).
   *
   * **`null` means unknown, not local.** Turns written before this was
   * recorded have no boundary, and stamping "on this device" on them would be
   * inventing a fact about a generation nobody observed.
   */
  readonly boundary?: string | null;
  readonly boundaryLabel?: string | null;
}

/** Mirrors `commands::StoredTurn`. */
export interface StoredTurn {
  readonly question: string;
  readonly answer: string;
  readonly thorough: boolean;
  readonly model: string | null;
  readonly scope: string | null;
  /**
   * Exactly what the answer cited, as it was cited. Not re-retrieved on
   * reopening: the chunk behind a citation can be superseded or its file
   * deleted in between, and a conversation that shows different sources than it
   * was answered from is worse than one that shows none.
   */
  readonly citations: readonly Citation[];
  readonly excluded: readonly ExcludedSource[];
  /**
   * Which projects the evidence came from. Derived in Rust from the stored
   * citations by the same rule the live answer uses, so a reopened conversation
   * says what it said the first time.
   */
  readonly projects: readonly string[];
  readonly usage: TurnUsage | null;
  readonly askedMs: number;
}

/** Mirrors `commands::ConversationDetail`. */
export interface ConversationDetail {
  readonly id: string;
  readonly title: string;
  readonly scope: string | null;
  readonly turns: readonly StoredTurn[];
}

/** Mirrors `commands::NewTurn` — a finished exchange on its way to disk. */
export interface NewTurn {
  readonly question: string;
  readonly answer: string;
  readonly thorough: boolean;
  readonly model: string | null;
  readonly scope: string | null;
  readonly citations: readonly Citation[];
  readonly excluded: readonly ExcludedSource[];
  readonly usage: TurnUsage | null;
}

/** Mirrors `commands::SavedTurn`. */
export interface SavedTurn {
  readonly id: string;
  readonly title: string;
}

export function listConversations(limit = 200): Promise<ConversationSummary[]> {
  return call<ConversationSummary[]>("list_conversations", { limit });
}

export function loadConversation(id: string): Promise<ConversationDetail> {
  return call<ConversationDetail>("load_conversation", { id });
}

/**
 * Persist a finished exchange.
 *
 * `conversation` is `null` for the first turn of a thread, and that first save
 * is what creates it — a "New conversation" button that wrote a row when it was
 * pressed would fill the list with threads nobody said anything in.
 */
export function saveTurn(
  conversation: string | null,
  turn: NewTurn,
): Promise<SavedTurn> {
  return call<SavedTurn>("save_turn", { conversation, turn });
}

export function renameConversation(id: string, title: string): Promise<void> {
  return call<void>("rename_conversation", { id, title });
}

/**
 * Take a conversation off the list.
 *
 * A **soft** delete: `status` moves to `DELETED` in the store and every row
 * stays where it is. A conversation is the one thing in that database which
 * cannot be re-derived from the user's files.
 */
export function deleteConversation(id: string): Promise<void> {
  return call<void>("delete_conversation", { id });
}

export function cancelAsk(id: string): Promise<boolean> {
  return call<boolean>("cancel_ask", { id });
}

export function releaseModel(): Promise<ModelsSnapshot> {
  return call<ModelsSnapshot>("release_model", {});
}
