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
    mutationFn: (body: { sha256: string; message?: string; override_reason?: string }) =>
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

/** What a review action changes: the draft's list row, its preview's approvals. */
function reviewMoved(
  queryClient: ReturnType<typeof useQueryClient>,
  org: string,
  db: string,
  id: string,
) {
  void queryClient.invalidateQueries({ queryKey: draftKeys.list(org, db), exact: true });
  void queryClient.invalidateQueries({ queryKey: draftKeys.one(org, db, id), exact: true });
  void queryClient.invalidateQueries({ queryKey: draftKeys.preview(org, db, id) });
}

/** Approves the SQL as this person sees it (`sha256` of it). */
export function useApproveDraft(org: string, db: string, id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (sha256: string) =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/drafts/{id}/approvals", {
          params: { path: { org, db, id } },
          body: { sha256 },
        }),
      ),
    onSettled: () => reviewMoved(queryClient, org, db, id),
  });
}

export function useWithdrawApproval(org: string, db: string, id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      unwrap(
        api.DELETE("/api/orgs/{org}/databases/{db}/drafts/{id}/approvals", {
          params: { path: { org, db, id } },
        }),
      ),
    onSettled: () => reviewMoved(queryClient, org, db, id),
  });
}

/** "Request review" (on) or take the request back (off). */
export function useReviewRequest(org: string, db: string, id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (on: boolean) => {
      const options = { params: { path: { org, db, id } } };
      const path = "/api/orgs/{org}/databases/{db}/drafts/{id}/review-request" as const;
      return unwrap(on ? api.POST(path, options) : api.DELETE(path, options));
    },
    onSettled: () => reviewMoved(queryClient, org, db, id),
  });
}
