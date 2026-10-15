/** True on Apple platforms, where shortcuts use ⌘ instead of Ctrl. */
export const isApple =
  typeof navigator !== "undefined" && /Mac|iPhone|iPad|iPod/.test(navigator.userAgent);

/** The label of the primary modifier key, for Kbd hints. */
export const modKey = isApple ? "⌘" : "Ctrl";
