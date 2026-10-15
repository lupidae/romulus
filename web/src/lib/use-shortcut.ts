import * as React from "react";

/** Whether a key press belongs to whatever has focus: a field, an editor, a menu. */
function typing(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false;
  return (
    target.isContentEditable ||
    ["INPUT", "TEXTAREA", "SELECT"].includes(target.tagName) ||
    target.closest("[role=dialog], [role=menu], [role=listbox], [role=application], .cm-editor") !==
      null
  );
}

/**
 * A single-key shortcut for the page (`u` uploads, `c` creates): ignored
 * while typing, in a dialog or menu, in the graph (which has its own keys),
 * and with a modifier held, so it never fights the browser or a field.
 */
export function useShortcut(key: string, handler: () => void, enabled = true) {
  const latest = React.useRef(handler);
  React.useEffect(() => {
    latest.current = handler;
  });
  React.useEffect(() => {
    if (!enabled) return;
    const onKey = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.repeat) return;
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      if (event.key !== key || typing(event.target)) return;
      event.preventDefault();
      latest.current();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [key, enabled]);
}
