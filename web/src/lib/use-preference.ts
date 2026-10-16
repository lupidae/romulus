import { useCallback, useState, useSyncExternalStore } from "react";

/**
 * A per-browser view preference (split ratio, hidden pane, the sidebar):
 * read once from localStorage, written on change. Storage can be missing or
 * throw (private windows); the preference then lasts for the page.
 */
export function usePreference<T>(key: string, fallback: T, valid: (value: unknown) => value is T) {
  const [value, setValue] = useState<T>(() => {
    try {
      const raw = localStorage.getItem(key);
      const parsed: unknown = raw === null ? undefined : JSON.parse(raw);
      return valid(parsed) ? parsed : fallback;
    } catch {
      return fallback;
    }
  });
  const set = useCallback(
    (next: T) => {
      setValue(next);
      try {
        localStorage.setItem(key, JSON.stringify(next));
      } catch {
        // Not persisted; still applied.
      }
    },
    [key],
  );
  return [value, set] as const;
}

/** Whether a media query matches, live. */
export function useMediaQuery(query: string): boolean {
  return useSyncExternalStore(
    (onChange) => {
      const mq = window.matchMedia(query);
      mq.addEventListener("change", onChange);
      return () => mq.removeEventListener("change", onChange);
    },
    () => window.matchMedia(query).matches,
    () => true,
  );
}
