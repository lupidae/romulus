import { useMutation, useQueryClient } from "@tanstack/react-query";

import { api, succeed, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

import { draftKeys } from "@/features/draft/queries";

import { type ApiToken, type Database, databaseKeys, type TokenScope } from "./queries";

type CreateDatabase = components["schemas"]["CreateDatabase"];
type UpdateDatabase = components["schemas"]["UpdateDatabase"];

const path = (org: string, db: string) => ({ params: { path: { org, db } } });

export function useCreateDatabase(org: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateDatabase) =>
      unwrap(api.POST("/api/orgs/{org}/databases", { params: { path: { org } }, body })),
    onSuccess: (database) => {
      queryClient.setQueryData(databaseKeys.one(org, database.slug), database);
      queryClient.setQueryData<Database[]>(databaseKeys.list(org), (list) =>
        list ? [database, ...list] : list,
      );
      void queryClient.invalidateQueries({ queryKey: databaseKeys.list(org), exact: true });
    },
  });
}

export function useUpdateDatabase(org: string, db: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: UpdateDatabase) =>
      unwrap(api.PATCH("/api/orgs/{org}/databases/{db}", { ...path(org, db), body })),
    onSuccess: (database) => {
      queryClient.setQueryData(databaseKeys.one(org, database.slug), database);
      if (database.slug !== db) queryClient.removeQueries({ queryKey: databaseKeys.one(org, db) });
      void queryClient.invalidateQueries({ queryKey: databaseKeys.list(org), exact: true });
    },
  });
}

export function useDeleteDatabase(org: string, db: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () => succeed(api.DELETE("/api/orgs/{org}/databases/{db}", path(org, db))),
    onSuccess: () => {
      queryClient.setQueryData<Database[]>(databaseKeys.list(org), (list) =>
        list?.filter((d) => d.slug !== db),
      );
      queryClient.removeQueries({ queryKey: databaseKeys.one(org, db) });
      void queryClient.invalidateQueries({ queryKey: databaseKeys.list(org), exact: true });
    },
  });
}

/** Uploads SQL as the next version; the database and its history follow. */
export function useUploadVersion(org: string, db: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: { sql: string; message?: string }) =>
      unwrap(api.POST("/api/orgs/{org}/databases/{db}/versions", { ...path(org, db), body })),
    onSuccess: (version) => {
      // The page shows the new version at once; the refetch confirms it.
      queryClient.setQueryData<Database>(
        databaseKeys.one(org, db),
        (database) =>
          database && { ...database, latest: version, last_activity_at: version.created_at },
      );
      void queryClient.invalidateQueries({ queryKey: databaseKeys.one(org, db) });
      void queryClient.invalidateQueries({ queryKey: databaseKeys.list(org), exact: true });
    },
  });
}

/** A dry run of an upload: errors, or what would change. */
export function useCompileSql(org: string, db: string) {
  return useMutation({
    mutationFn: (sql: string) =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/compile", { ...path(org, db), body: { sql } }),
      ),
  });
}

export function useCreateToken(org: string, db: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: { name: string; scope: TokenScope }) =>
      unwrap(api.POST("/api/orgs/{org}/databases/{db}/tokens", { ...path(org, db), body })),
    onSuccess: ({ token }) => {
      queryClient.setQueryData<ApiToken[]>(databaseKeys.tokens(org, db), (tokens) =>
        tokens ? [token, ...tokens] : tokens,
      );
      void queryClient.invalidateQueries({ queryKey: databaseKeys.tokens(org, db) });
    },
  });
}

/** Revokes at once in the list, and puts the token back if the API refuses. */
export function useRevokeToken(org: string, db: string) {
  const queryClient = useQueryClient();
  const key = databaseKeys.tokens(org, db);
  return useMutation({
    mutationFn: (id: string) =>
      succeed(
        api.DELETE("/api/orgs/{org}/databases/{db}/tokens/{id}", {
          params: { path: { org, db, id } },
        }),
      ),
    onMutate: async (id) => {
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<ApiToken[]>(key);
      queryClient.setQueryData<ApiToken[]>(key, (tokens) => tokens?.filter((t) => t.id !== id));
      return { previous };
    },
    onError: (_error, _id, context) => queryClient.setQueryData(key, context?.previous),
    onSettled: () => void queryClient.invalidateQueries({ queryKey: key }),
  });
}

/** Everything a removal or a restore changes: the latest version, the history, drafts' bases. */
function versionsMoved(queryClient: ReturnType<typeof useQueryClient>, org: string, db: string) {
  for (const key of [
    databaseKeys.one(org, db),
    databaseKeys.versions(org, db),
    databaseKeys.removed(org, db),
    databaseKeys.activity(org, db),
    draftKeys.list(org, db),
  ])
    void queryClient.invalidateQueries({ queryKey: key });
  void queryClient.invalidateQueries({ queryKey: databaseKeys.list(org), exact: true });
}

/** Removes a version (admins), saying why. */
export function useRemoveVersion(org: string, db: string, databaseId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: ({ number, reason }: { number: number; reason: string }) =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/versions/{number}/remove", {
          params: { path: { org, db, number } },
          body: { reason },
        }),
      ),
    onSuccess: (_removed, { number }) => {
      // Cached forever while it was live; gone now.
      queryClient.removeQueries({ queryKey: ["versions", databaseId, number] });
      versionsMoved(queryClient, org, db);
    },
  });
}

/** Restores a removed version (admins, within 30 days). */
export function useRestoreVersion(org: string, db: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (number: number) =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/versions/{number}/restore", {
          params: { path: { org, db, number } },
        }),
      ),
    onSuccess: () => versionsMoved(queryClient, org, db),
  });
}
