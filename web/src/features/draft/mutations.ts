import { useMutation, useQueryClient } from "@tanstack/react-query";

import { api, succeed, unwrap } from "@/api/client";
import { databaseKeys } from "@/features/databases/queries";

import { type Draft, draftKeys } from "./queries";

/** One click: a draft from the latest version, ready to open. */
export function useCreateDraft(org: string, db: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      unwrap(api.POST("/api/orgs/{org}/databases/{db}/drafts", { params: { path: { org, db } } })),
    onSuccess: (draft) => {
      queryClient.setQueryData(draftKeys.one(org, db, draft.id), draft);
      queryClient.setQueryData<Draft[]>(draftKeys.list(org, db), (drafts) =>
        drafts ? [draft, ...drafts] : drafts,
      );
      void queryClient.invalidateQueries({ queryKey: draftKeys.list(org, db), exact: true });
    },
  });
}

/** Renames at once, and puts the name back if the API refuses. */
export function useRenameDraft(org: string, db: string, id: string) {
  const queryClient = useQueryClient();
  const key = draftKeys.one(org, db, id);
  return useMutation({
    mutationFn: (name: string) =>
      unwrap(
        api.PATCH("/api/orgs/{org}/databases/{db}/drafts/{id}", {
          params: { path: { org, db, id } },
          body: { name },
        }),
      ),
    onMutate: async (name) => {
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<Draft>(key);
      queryClient.setQueryData<Draft>(key, (draft) => (draft ? { ...draft, name } : draft));
      return { previous };
    },
    onError: (_error, _name, context) => queryClient.setQueryData(key, context?.previous),
    onSuccess: (draft) => queryClient.setQueryData(key, draft),
    onSettled: () =>
      void queryClient.invalidateQueries({ queryKey: draftKeys.list(org, db), exact: true }),
  });
}

export function useDeleteDraft(org: string, db: string, id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      succeed(
        api.DELETE("/api/orgs/{org}/databases/{db}/drafts/{id}", {
          params: { path: { org, db, id } },
        }),
      ),
    onSuccess: () => {
      queryClient.setQueryData<Draft[]>(draftKeys.list(org, db), (drafts) =>
        drafts?.filter((d) => d.id !== id),
      );
      queryClient.removeQueries({ queryKey: draftKeys.one(org, db, id) });
    },
  });
}

/** "Update draft": brings the latest version in (conflicts left as blocks). */
export function useUpdateDraft(org: string, db: string, id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/drafts/{id}/update", {
          params: { path: { org, db, id } },
        }),
      ),
    onSettled: () => {
      void queryClient.invalidateQueries({ queryKey: draftKeys.one(org, db, id) });
      void queryClient.invalidateQueries({ queryKey: draftKeys.preview(org, db, id) });
      void queryClient.invalidateQueries({ queryKey: draftKeys.list(org, db), exact: true });
    },
  });
}

export function usePublishDraft(org: string, db: string, id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: { sha256: string; message?: string }) =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/drafts/{id}/publish", {
          params: { path: { org, db, id } },
          body,
        }),
      ),
    onSuccess: () => {
      // Not the preview: the draft is published, there is nothing to preview.
      queryClient.removeQueries({ queryKey: draftKeys.preview(org, db, id) });
      void queryClient.invalidateQueries({ queryKey: draftKeys.list(org, db), exact: true });
      void queryClient.invalidateQueries({ queryKey: draftKeys.one(org, db, id), exact: true });
      // The database's latest version and history now include it.
      void queryClient.invalidateQueries({ queryKey: databaseKeys.one(org, db) });
      void queryClient.invalidateQueries({ queryKey: databaseKeys.versions(org, db) });
      void queryClient.invalidateQueries({ queryKey: databaseKeys.list(org) });
    },
  });
}

/**
 * Rewrites the draft's SQL in canonical form, on the server: everyone in the
 * draft sees the rewrite arrive like any edit.
 */
export function useTidyDraft(org: string, db: string, id: string) {
  return useMutation({
    mutationFn: () =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/drafts/{id}/tidy", {
          params: { path: { org, db, id } },
        }),
      ),
  });
}
