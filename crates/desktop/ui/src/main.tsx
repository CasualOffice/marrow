import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";

import "./styles/global.css";
import { App } from "./App";
import { applyTheme, loadTheme, watchSystemTheme } from "./theme";
import { useUi } from "./store";

/**
 * One client, no devtools, no retries. Every query in this app is a local IPC
 * call to a synchronous core: if it fails it fails deterministically, and
 * retrying only delays the error message the user needs to read.
 */
const client = new QueryClient({
  defaultOptions: {
    queries: {
      retry: false,
      refetchOnWindowFocus: false,
      refetchOnReconnect: false,
      networkMode: "always",
    },
  },
});

/*
 * Resolve the theme before the first paint, so a dark-mode launch never flashes
 * the light palette. `tokens.css` keys dark off `[data-theme]` alone, and this
 * is the only place that attribute is written.
 */
applyTheme(loadTheme());
watchSystemTheme(() => useUi.getState().theme);

const root = document.getElementById("root");
if (!root) throw new Error("index.html is missing #root");

createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={client}>
      <App />
    </QueryClientProvider>
  </StrictMode>,
);
