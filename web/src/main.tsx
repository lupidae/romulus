import "./styles/globals.css";

import { MutationCache, QueryCache, QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRouter, RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { ApiError } from "./api/client";
import { meQuery } from "./features/auth/queries";
import { orgKeys } from "./features/orgs/queries";
import { routeTree } from "./routeTree.gen";

/**
 * Errors any screen can meet. A 401 means the session ended (expired, or
 * signed out in another tab): go to sign-in and come back here afterwards.
 * Route guards handle the `me` query themselves. A 403 may mean an admin just
 * changed my role: refetch what I may do, so the screen catches up.
 */
function onApiError(error: unknown, queryKey?: readonly unknown[]) {
  if (!(error instanceof ApiError)) return;
  if (error.status === 401 && queryKey?.[0] !== "me") {
    const { pathname, search, hash } = window.location;
    if (pathname.startsWith("/app/signin")) return;
    // The cached user is no longer signed in: without this, sign-in would
    // find it and send them straight back here, and round it goes.
    queryClient.removeQueries({ queryKey: meQuery.queryKey });
    void router.navigate({
      to: "/signin",
      search: {
        next: `${pathname}${search}${hash}`,
        error: error.code === "session_expired" ? "session_expired" : undefined,
      },
    });
  } else if (error.status === 403) {
    void queryClient.invalidateQueries({ queryKey: orgKeys.all });
  }
}

const queryClient = new QueryClient({
  queryCache: new QueryCache({ onError: (error, query) => onApiError(error, query.queryKey) }),
  mutationCache: new MutationCache({ onError: (error) => onApiError(error) }),
  defaultOptions: {
    queries: {
      staleTime: 30_000,
      // A 4xx will not fix itself; retrying only delays the error state.
      retry: (failures, error) =>
        !(error instanceof ApiError && error.status < 500) && failures < 2,
    },
  },
});

const router = createRouter({
  routeTree,
  // Matches `base` in vite.config.ts: the SPA lives under /app.
  basepath: "/app",
  context: { queryClient },
  defaultPreload: "intent",
  // Loaders read through TanStack Query, which owns caching.
  defaultPreloadStaleTime: 0,
  scrollRestoration: true,
  // A path no page has (/app/acme/billing/typo) is the app's 404, for
  // everyone; only a loader that looked something up and found nothing
  // (an unknown database) shows its route's own not-found state. The
  // default, "fuzzy", would render it inside the deepest matching layout,
  // behind that layout's sign-in check.
  notFoundMode: "root",
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
  interface StaticDataRouteOption {
    /** The page lays itself out edge to edge in the shell, without `PageBody`. */
    fullBleed?: boolean;
  }
}

const root = document.getElementById("root");
if (!root) throw new Error("index.html has no #root");

createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
