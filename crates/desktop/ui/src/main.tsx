import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";

import "./styles/global.css";
import { App } from "./App";
// One client, and it lives in its own module because `actions.ts` needs it too
// — granting a folder or dropping a file changes what the core holds, and the
// panels reading the core have to be told.
import { queryClient as client } from "./queryClient";
import { applyTheme, loadTheme, watchSystemTheme } from "./theme";
import { useUi } from "./store";

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
