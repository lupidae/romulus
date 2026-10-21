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

export type ConnectedApp = components["schemas"]["ConnectedApp"];
export type AuthorizationRequest = components["schemas"]["AuthorizationRequest"];

export const connectedAppKeys = {
  all: ["connected-apps"] as const,
};

/** The apps the signed-in person connected (OAuth), most recently used first. */
export const connectedAppsQuery = queryOptions({
  queryKey: connectedAppKeys.all,
  queryFn: () => unwrap(api.GET("/api/me/connected-apps")),
});

/**
 * An app's request to connect, as this browser started it (the consent
 * page). Answered once: never refetched behind the person's back.
 */
export const authorizationRequestQuery = (id: string) =>
  queryOptions({
    queryKey: ["oauth-request", id],
    queryFn: () => unwrap(api.GET("/api/oauth/requests/{id}", { params: { path: { id } } })),
    staleTime: Infinity,
    retry: false,
  });
