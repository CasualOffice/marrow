/**
 * Server state. Every panel is a query against the local core, which is exactly
 * TanStack Query's job (GUI §2).
 *
 * The whole file is shaped by one budget: keystroke -> first lexical result in
 * under 50 ms (GUI §7). That means no retry (a local IPC call that fails
 * deterministically fails again), no refetch-on-focus churn, and
 * `keepPreviousData` everywhere so a known result is never replaced by a
 * loading state — "no spinner ever gates a result that is already known".
 */

import { useEffect, useRef, useState } from "react";
import {
  keepPreviousData,
  useQuery,
  type UseQueryResult,
} from "@tanstack/react-query";

import type { Region } from "./api";
import {
  asUiError,
  fileDetail,
  indexHealth,
  listConversations,
  listFiles,
  listProjects,
  listWorkspaces,
  modelsOverview,
  providerSettings,
  readRegion,
  scratchStatus,
  search,
  type ConversationSummary,
  type FileDetail,
  type FileRow,
  type IndexHealth,
  type ScratchStatus,
  type SearchResponse,
  type UiError,
  type WorkspaceRow,
} from "./api";

/**
 * GUI §7: "Search input is not debounced beyond 30 ms. The index is faster than
 * the debounce would be." Clearing to empty is not debounced at all — there is
 * nothing to coalesce and an empty field should stop showing results now.
 */
export const DEBOUNCE_MS = 30;

export function useDebounced(value: string, delay = DEBOUNCE_MS): string {
  const [settled, setSettled] = useState(value);
  useEffect(() => {
    if (value.trim() === "") {
      setSettled(value);
      return;
    }
    const t = window.setTimeout(() => setSettled(value), delay);
    return () => window.clearTimeout(t);
  }, [value, delay]);
  return settled;
}

/** Default result budget. `⌘1–9` addresses the first nine of them. */
export const SEARCH_LIMIT = 200;

export type SearchQuery = UseQueryResult<SearchResponse, UiError>;

export function useSearch(query: string, limit = SEARCH_LIMIT): SearchQuery {
  const trimmed = query.trim();
  return useQuery<SearchResponse, UiError>({
    queryKey: ["search", trimmed, limit],
    queryFn: () => search(trimmed, limit).catch((e) => Promise.reject(asUiError(e))),
    enabled: trimmed.length > 0,
    // The list is never emptied while a *newer query* is in flight: the previous
    // ranking stays on screen and is replaced in place when the new one lands.
    // Clearing the field is not that case — an empty query has no ranking, and
    // holding the old one there would be showing an answer to a question the
    // user just withdrew.
    ...(trimmed.length > 0 ? { placeholderData: keepPreviousData } : {}),
    staleTime: 15_000,
    gcTime: 5 * 60_000,
    retry: false,
  });
}

export function useProjects() {
  return useQuery({
    queryKey: ["projects"],
    queryFn: listProjects,
    // The set only changes when a folder is indexed, so this is cheap to hold
    // and expensive to recompute — it walks every indexed path.
    staleTime: 60_000,
    retry: false,
  });
}

/** The key everything that changes a conversation invalidates. */
export const CONVERSATIONS_KEY = ["conversations"] as const;

/**
 * The thread list in the sidebar.
 *
 * Not polled. Conversations only change because *this* window changed them —
 * a turn finished, a rename, a delete — and every one of those invalidates the
 * key itself. An interval here would be a query re-running all day to discover
 * what it already did.
 */
export function useConversations(): UseQueryResult<
  ConversationSummary[],
  UiError
> {
  return useQuery<ConversationSummary[], UiError>({
    queryKey: CONVERSATIONS_KEY,
    queryFn: () =>
      listConversations().catch((e) => Promise.reject(asUiError(e))),
    staleTime: 60_000,
    retry: false,
  });
}

export function useWorkspaces(): UseQueryResult<WorkspaceRow[], UiError> {
  return useQuery<WorkspaceRow[], UiError>({
    queryKey: ["workspaces"],
    queryFn: () => listWorkspaces().catch((e) => Promise.reject(asUiError(e))),
    staleTime: 20_000,
    // A degraded workspace must become visible in the sidebar without
    // navigating (GUI §11), so this polls rather than waiting for a visit.
    refetchInterval: 30_000,
    retry: false,
  });
}

/** How many files the browser lists at once. Newest first, so this is a window
 *  onto the top of the index rather than a page through all of it. */
export const FILES_LIMIT = 500;

/**
 * The Files browser.
 *
 * Unconditional: no query, no `enabled` gate. That is the whole point — the
 * previous version was built on `search`, which returns nothing without a
 * query, so a 35,000-file index rendered an empty column.
 *
 * The prefix is debounced like the search field and for the same reason, but a
 * cleared box is not debounced at all: there is nothing to coalesce and the
 * full list should come back immediately.
 */
export function useFiles(
  workspace: string | null,
  prefix: string,
  limit = FILES_LIMIT,
): UseQueryResult<FileRow[], UiError> {
  const p = prefix.trim();
  return useQuery<FileRow[], UiError>({
    queryKey: ["files", workspace, p, limit],
    queryFn: () =>
      listFiles({
        workspace: workspace ?? undefined,
        prefix: p === "" ? undefined : p,
        limit,
      }).catch((e) => Promise.reject(asUiError(e))),
    // A known list is never replaced by a loading state while a narrower one
    // is in flight (GUI §5.2, "no spinner ever gates a result already known").
    placeholderData: keepPreviousData,
    staleTime: 15_000,
    gcTime: 5 * 60_000,
    retry: false,
  });
}

/** The key a drop invalidates, along with workspaces, files and health. */
export const SCRATCH_KEY = ["scratch"] as const;

/**
 * What is in the dropped-files folder.
 *
 * Not polled. It only changes because this window changed it — a drop, a pick,
 * an emptying — and each of those invalidates the key. An interval here would
 * be a `read_dir` running all day to rediscover what it already knows.
 */
export function useScratch(): UseQueryResult<ScratchStatus, UiError> {
  return useQuery<ScratchStatus, UiError>({
    queryKey: SCRATCH_KEY,
    queryFn: () => scratchStatus().catch((e) => Promise.reject(asUiError(e))),
    staleTime: 60_000,
    retry: false,
  });
}

export function useIndexHealth(): UseQueryResult<IndexHealth, UiError> {
  return useQuery<IndexHealth, UiError>({
    queryKey: ["health"],
    queryFn: () => indexHealth().catch((e) => Promise.reject(asUiError(e))),
    staleTime: 20_000,
    refetchInterval: 30_000,
    retry: false,
  });
}

export function useFileDetail(
  path: string | null,
): UseQueryResult<FileDetail, UiError> {
  return useQuery<FileDetail, UiError>({
    queryKey: ["file", path],
    queryFn: () =>
      fileDetail(path as string).catch((e) => Promise.reject(asUiError(e))),
    enabled: path !== null,
    placeholderData: keepPreviousData,
    staleTime: 30_000,
    retry: false,
  });
}

export type { Region };

export function useRegion(
  path: string | null,
  aroundLine: number | null,
): UseQueryResult<Region, UiError> {
  const around = aroundLine ?? undefined;
  return useQuery<Region, UiError>({
    queryKey: ["region", path, around ?? null],
    // The core now returns `firstLine` itself, so nothing here reconstructs it
    // from a duplicated constant.
    queryFn: () =>
      readRegion(path as string, around).catch((e) => Promise.reject(asUiError(e))),
    enabled: path !== null,
    placeholderData: keepPreviousData,
    staleTime: 30_000,
    retry: false,
  });
}

/**
 * True once `value` has been non-transiently true for `ms`. Used for the "a
 * slow branch shows a subtle inline indicator in the footer" rule (GUI §5.2) —
 * a fetch that lands inside the budget must never flash an indicator.
 */
export function useSettledFlag(value: boolean, ms: number): boolean {
  const [flag, setFlag] = useState(false);
  const timer = useRef<number>(0);
  useEffect(() => {
    if (!value) {
      window.clearTimeout(timer.current);
      setFlag(false);
      return;
    }
    timer.current = window.setTimeout(() => setFlag(true), ms);
    return () => window.clearTimeout(timer.current);
  }, [value, ms]);
  return flag;
}

/**
 * The Models page.
 *
 * Refetched on an interval because the numbers on it are *live*: available
 * memory moves, and Ollama can start or stop while the page is open. A
 * recommendation made at launch is wrong by the time it is acted on (LLM-019).
 * Four seconds is twice the sampler's own interval — fast enough that the
 * figure is never visibly stale, slow enough that the page is not the reason
 * the machine is busy.
 */
export function useModels() {
  return useQuery({
    queryKey: ["models"],
    queryFn: modelsOverview,
    refetchInterval: 4_000,
    staleTime: 2_000,
    retry: false,
  });
}

export const PROVIDER_KEY = ["provider"] as const;

/**
 * The answering endpoint, as Settings shows it.
 *
 * Separate from `useModels` rather than folded into it, because it answers a
 * question neither half can answer alone: the hub knows the endpoint and the
 * index knows the workspace classifications, and "this is configured and will
 * still be refused" (LLM-032) needs both. **Not** on a four-second interval:
 * it resolves a hostname and asks the keychain, and nothing on it changes
 * unless this page changes it.
 */
export function useProviderSettings() {
  return useQuery({
    queryKey: PROVIDER_KEY,
    queryFn: providerSettings,
    staleTime: 30_000,
    retry: false,
  });
}
