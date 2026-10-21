import { queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type AccessToken = components["schemas"]["AccessToken"];
export type CreatedAccessToken = components["schemas"]["CreatedAccessToken"];
export type AccessTokenScope = components["schemas"]["AccessTokenScope"];

export const accessTokenKeys = {
  all: ["access-tokens"] as const,
};

/** The signed-in person's access tokens (not revoked), newest first. */
export const accessTokensQuery = queryOptions({
  queryKey: accessTokenKeys.all,
  queryFn: () => unwrap(api.GET("/api/me/access-tokens")),
});
