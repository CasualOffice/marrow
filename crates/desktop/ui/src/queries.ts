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
  listWorkspaces,
  readRegion,
  search,
  type FileDetail,
  type IndexHealth,
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
