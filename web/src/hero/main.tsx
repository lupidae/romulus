// The live hero's entry, loaded by the homepage (site/public/app.js) once the
// page is idle, or when a visitor asks for it on a phone. `mount` resolves
// once the wasm is ready and the island is drawn, so the page can swap its
// static picture for it in one step; it rejects if anything fails, and the
// picture stays. A failure after that (a render that throws, an engine that
// cannot restart) forgets what this browser kept, takes the island down and
// calls `onCrash`: the page shows its picture again, with a note.

import "./hero.css";

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { PortalContainerProvider } from "@/components/ui/portal";
import { TooltipProvider } from "@/components/ui/tooltip";

import { HeroBoundary } from "./boundary";
import { containScroll } from "./contain-scroll";
import { loadEngine } from "./engine";
import { HeroApp } from "./hero-app";
import * as store from "./store";

export interface Mounted {
  unmount: () => void;
}

export interface MountOptions {
  /** The island failed after it was drawn and is gone. */
  onCrash?: (error: unknown) => void;
}

/**
 * Draws the hero in `root`; its menus, popovers and dialogs go in `layers`
 * (outside the product window, which clips and contains what is inside it).
 * Both must carry the `romulus-hero` class, which the stylesheet is scoped to.
 */
export async function mount(
  root: HTMLElement,
  layers: HTMLElement,
  { onCrash }: MountOptions = {},
): Promise<Mounted> {
  const engine = await loadEngine();
  containScroll(".romulus-hero");
  const queryClient = new QueryClient({
    defaultOptions: { queries: { retry: false, refetchOnWindowFocus: false } },
  });
  const reactRoot = createRoot(root);
  let drawn = false;
  let failure: { error: unknown } | null = null;
  let stopFatal = () => {};
  const crash = (error: unknown) => {
    if (failure) return;
    failure = { error };
    console.error("romulus: the live editor stopped", error);
    // What it kept may be what broke it: the next try starts over.
    store.clear();
    stopFatal();
    // Not while React is still rendering or committing.
    setTimeout(() => reactRoot.unmount());
    if (drawn) onCrash?.(error);
  };
  stopFatal = engine.onFatal(crash);
  reactRoot.render(
    <StrictMode>
      <HeroBoundary onError={crash}>
        <QueryClientProvider client={queryClient}>
          <PortalContainerProvider value={layers}>
            <TooltipProvider>
              <HeroApp engine={engine} />
            </TooltipProvider>
          </PortalContainerProvider>
        </QueryClientProvider>
      </HeroBoundary>
    </StrictMode>,
  );
  // Drawn: the next frame shows it.
  await new Promise((resolve) => requestAnimationFrame(() => requestAnimationFrame(resolve)));
  // Failed while drawing: the page keeps its picture (mount rejects).
  if (failure) throw (failure as { error: unknown }).error;
  drawn = true;
  return {
    unmount: () => {
      stopFatal();
      reactRoot.unmount();
    },
  };
}
