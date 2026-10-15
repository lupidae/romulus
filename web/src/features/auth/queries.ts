import { queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type Me = components["schemas"]["CurrentUser"];

/** The signed-in user; a 401 means signed out (`session_expired` if a session lapsed). */
export const meQuery = queryOptions({
  queryKey: ["me"],
  queryFn: () => unwrap(api.GET("/api/me")),
  staleTime: 60_000,
});

/** What the sign-in page can offer: SSO, and the dev sign-in on local stacks. */
export const authMethodsQuery = queryOptions({
  queryKey: ["auth", "methods"],
  queryFn: () => unwrap(api.GET("/api/auth/methods")),
  staleTime: Infinity,
});
