import { useCallback, useSyncExternalStore } from "react";

export type ThemePreference = "system" | "light" | "dark";

// Must match the inline script in index.html, which applies the theme before
// first paint so a dark-mode user never sees a white flash.
const STORAGE_KEY = "romulus-theme";
const media = () => window.matchMedia("(prefers-color-scheme: dark)");

function read(): ThemePreference {
  try {
    const value = localStorage.getItem(STORAGE_KEY);
    return value === "light" || value === "dark" ? value : "system";
  } catch {
    return "system";
  }
}

export function applyTheme(preference: ThemePreference) {
  const dark = preference === "dark" || (preference === "system" && media().matches);
  document.documentElement.classList.toggle("dark", dark);
}

const listeners = new Set<() => void>();

function subscribe(listener: () => void) {
  listeners.add(listener);
  const onSystemChange = () => {
    if (read() === "system") applyTheme("system");
    listener();
  };
  const mq = media();
  mq.addEventListener("change", onSystemChange);
  return () => {
    listeners.delete(listener);
    mq.removeEventListener("change", onSystemChange);
  };
}

/** The user's theme choice, persisted per browser. */
export function useTheme() {
  const preference = useSyncExternalStore(subscribe, read, () => "system" as const);
  const setPreference = useCallback((next: ThemePreference) => {
    try {
      if (next === "system") localStorage.removeItem(STORAGE_KEY);
      else localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // Private mode: the choice lasts for this page only.
    }
    applyTheme(next);
    listeners.forEach((l) => l());
  }, []);
  return { preference, setPreference };
}
