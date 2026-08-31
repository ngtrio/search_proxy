import { useEffect, useState } from "react";

export type Theme = "dark" | "light";
const themeKey = "search-proxy-theme";
export const themeBackgrounds: Record<Theme, string> = {
  dark: "#0a0c0d",
  light: "#f3f2ed",
};

// This runs as the first element in <head>, before stylesheets and hydration.
// Keep it self-contained: the browser executes the string without module scope.
export const THEME_BOOTSTRAP_SCRIPT = `(() => {
  let theme;
  try {
    const stored = localStorage.getItem("${themeKey}");
    theme = stored === "dark" || stored === "light"
      ? stored
      : (matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark");
  } catch {
    theme = matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
  }
  const root = document.documentElement;
  root.dataset.theme = theme;
  root.style.backgroundColor = theme === "light" ? "${themeBackgrounds.light}" : "${themeBackgrounds.dark}";
  root.style.colorScheme = theme;
})();`;

function preferredTheme(): Theme {
  try {
    const stored = window.localStorage.getItem(themeKey);
    if (stored === "dark" || stored === "light") return stored;
  } catch {
    // System preference remains available when storage is blocked.
  }
  return window.matchMedia("(prefers-color-scheme: light)").matches ? "light" : "dark";
}

function applyTheme(theme: Theme) {
  const root = document.documentElement;
  root.dataset.theme = theme;
  root.style.backgroundColor = themeBackgrounds[theme];
  root.style.colorScheme = theme;
  document.querySelectorAll<HTMLMetaElement>('meta[name="theme-color"]').forEach(meta => {
    meta.content = themeBackgrounds[theme];
  });
}

export function useTheme() {
  const [state, setState] = useState<{ theme: Theme; ready: boolean }>({ theme: "dark", ready: false });
  useEffect(() => {
    const theme = preferredTheme();
    applyTheme(theme);
    setState({ theme, ready: true });
  }, []);
  useEffect(() => {
    if (!state.ready) return;
    applyTheme(state.theme);
    try { window.localStorage.setItem(themeKey, state.theme); } catch { /* Theme still applies for this visit. */ }
  }, [state]);
  const setTheme = (theme: Theme) => setState({ theme, ready: true });
  return [state.theme, setTheme] as const;
}
