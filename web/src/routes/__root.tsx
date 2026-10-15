import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, HeadContent, Outlet } from "@tanstack/react-router";
import { lazy, Suspense } from "react";

import { ErrorPage, NotFoundPage } from "@/components/shell/status-page";
import { Toaster } from "@/components/ui/toaster";
import { TooltipProvider } from "@/components/ui/tooltip";

export interface RouterContext {
  queryClient: QueryClient;
}

const Devtools = import.meta.env.DEV
  ? lazy(() => import("@/components/shell/devtools"))
  : () => null;

export const Route = createRootRouteWithContext<RouterContext>()({
  head: () => ({ meta: [{ title: "romulus" }] }),
  component: Root,
  notFoundComponent: NotFoundPage,
  errorComponent: ErrorPage,
});

function Root() {
  return (
    <TooltipProvider>
      <HeadContent />
      <Outlet />
      <Toaster />
      <Suspense>
        <Devtools />
      </Suspense>
    </TooltipProvider>
  );
}
