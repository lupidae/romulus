import * as React from "react";

/**
 * Where the kit's floating layers (tooltips, menus, popovers, selects,
 * dialogs, sheets) render: `document.body`, unless an ancestor provides an
 * element. The homepage's live hero (src/hero) renders them in its own
 * element, which its scoped stylesheet reaches.
 */
const PortalContainer = React.createContext<HTMLElement | null>(null);

export const PortalContainerProvider = PortalContainer.Provider;

/** A Radix `Portal`'s `container`: the provided element, else the body. */
export function usePortalContainer(): HTMLElement | undefined {
  return React.useContext(PortalContainer) ?? undefined;
}
