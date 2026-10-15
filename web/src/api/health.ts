import { queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "./client";

/** Whether the API and its database answer. The sign-in page uses it before sending anyone to SSO. */
export const healthQuery = queryOptions({
  queryKey: ["health"],
  queryFn: () => unwrap(api.GET("/api/health")),
  retry: false,
  staleTime: 10_000,
});
