import { useMutation, useQueryClient } from "@tanstack/react-query";

import { api, succeed, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

import { type AccessToken, accessTokenKeys, type ConnectedApp, connectedAppKeys } from "./queries";

type CreateAccessToken = components["schemas"]["CreateAccessToken"];
type Decision = components["schemas"]["Decision"];

export function useCreateAccessToken() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateAccessToken) => unwrap(api.POST("/api/me/access-tokens", { body })),
    onSuccess: (created) => {
      queryClient.setQueryData<AccessToken[]>(accessTokenKeys.all, (tokens) =>
        tokens ? [created.token, ...tokens] : tokens,
      );
      void queryClient.invalidateQueries({ queryKey: accessTokenKeys.all });
    },
  });
}

/** Revokes a token: gone from the list at once, back if the API refuses. */
export function useRevokeAccessToken() {
  const queryClient = useQueryClient();
  const key = accessTokenKeys.all;
  return useMutation({
    mutationFn: (id: string) =>
      succeed(api.DELETE("/api/me/access-tokens/{id}", { params: { path: { id } } })),
    onMutate: async (id) => {
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<AccessToken[]>(key);
      queryClient.setQueryData<AccessToken[]>(key, (tokens) => tokens?.filter((t) => t.id !== id));
      return { previous };
    },
    onError: (_error, _id, context) => queryClient.setQueryData(key, context?.previous),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: key }),
  });
}

/** Disconnects an app: gone from the list at once, back if the API refuses. */
export function useDisconnectApp() {
  const queryClient = useQueryClient();
  const key = connectedAppKeys.all;
  return useMutation({
    mutationFn: (clientId: string) =>
      succeed(
        api.DELETE("/api/me/connected-apps/{client_id}", {
          params: { path: { client_id: clientId } },
        }),
      ),
    onMutate: async (clientId) => {
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<ConnectedApp[]>(key);
      queryClient.setQueryData<ConnectedApp[]>(key, (apps) =>
        apps?.filter((a) => a.client_id !== clientId),
      );
      return { previous };
    },
    onError: (_error, _id, context) => queryClient.setQueryData(key, context?.previous),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: key }),
  });
}

/** The person's answer to an app's request: where to send the browser next. */
export function useDecideAuthorization(id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (decision: Decision) =>
      unwrap(api.POST("/api/oauth/requests/{id}", { params: { path: { id } }, body: decision })),
    onSuccess: () => void queryClient.invalidateQueries({ queryKey: connectedAppKeys.all }),
  });
}
