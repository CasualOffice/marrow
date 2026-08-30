/**
 * The one query client.
 *
 * In its own module rather than in `main.tsx` because things that are not
 * components need it: granting a folder, picking files and emptying the
 * dropped-files folder all change what the *core* holds, and every panel that
 * reads the core is a query. Those verbs live in `actions.ts` precisely so the
 * keyboard path and the mouse path are the same function, and a function that
 * cannot invalidate is one that leaves four panels showing what was true a
 * moment ago.
 *
 * Importing it from `main.tsx` instead would be a cycle: `main` renders `App`,
 * `App` reaches `actions`, `actions` would reach back into `main`.
 *
 * No retries, no refetch on focus. Every query here is a local IPC call into a
 * synchronous core: if it fails it fails deterministically, and retrying only
 * delays the message the user needs to read.
 */

import { QueryClient } from "@tanstack/react-query";

export const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      retry: false,
      refetchOnWindowFocus: false,
      refetchOnReconnect: false,
      networkMode: "always",
    },
  },
});
