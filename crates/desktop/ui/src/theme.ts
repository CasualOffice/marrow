/**
 * Appearance.
 *
 * The one setting this build genuinely has, so it is the one Settings renders
 * as a live control rather than as a sentence. It is entirely local: there is
 * no desktop command behind it and none is needed — the choice is a property of
 * this window, not of the index.
 *
 * Resolution happens here rather than in a media query because `tokens.css`
 * keys its dark palette off `[data-theme="dark"]` and nothing else. A second
 * copy of the palette behind `prefers-color-scheme` is a second thing to drift
 * from the design reference.
 */

export type ThemeChoice = "system" | "light" | "dark";

const KEY = "marrow.theme";

export function isThemeChoice(v: unknown): v is ThemeChoice {
  return v === "system" || v === "light" || v === "dark";
}

/** What is stored, defaulting to following the system. */
export function loadTheme(): ThemeChoice {
  try {
    const v = window.localStorage.getItem(KEY);
    return isThemeChoice(v) ? v : "system";
  } catch {
    // A WebView with storage denied is not a reason to fail to start.
    return "system";
  }
}

export function saveTheme(choice: ThemeChoice): void {
  try {
    window.localStorage.setItem(KEY, choice);
  } catch {
    /* the attribute below is still applied; only persistence is lost */
  }
}

/** What `system` currently means. */
export function systemTheme(): "light" | "dark" {
  return window.matchMedia("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

export function resolveTheme(choice: ThemeChoice): "light" | "dark" {
  return choice === "system" ? systemTheme() : choice;
}

/**
 * Write the resolved theme onto `<html>`.
 *
 * Always both attributes' worth of information: `data-theme` is the palette
 * switch and `data-theme-choice` records whether the user picked it, so the
 * Settings control can show "system" as selected while the page renders dark.
 */
export function applyTheme(choice: ThemeChoice): void {
  const el = document.documentElement;
  el.setAttribute("data-theme", resolveTheme(choice));
  el.setAttribute("data-theme-choice", choice);
}

/** Re-apply when the system flips, but only while the choice is `system`. */
export function watchSystemTheme(get: () => ThemeChoice): () => void {
  const mq = window.matchMedia("(prefers-color-scheme: dark)");
  const onChange = () => {
    if (get() === "system") applyTheme("system");
  };
  mq.addEventListener("change", onChange);
  return () => mq.removeEventListener("change", onChange);
}
