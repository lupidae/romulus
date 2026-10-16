import { queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type Draft = components["schemas"]["Draft"];
export type Present = components["schemas"]["Present"];
export type PublishPreview = components["schemas"]["PublishPreview"];
export type Diagnostic = components["schemas"]["Diagnostic"];
export type ServerEvent = components["schemas"]["ServerEvent"];
export type LiveStatus = components["schemas"]["LiveStatus"];
export type CompileState = components["schemas"]["CompileState"];
export type DraftMode = components["schemas"]["DraftMode"];
export type WrittenMigration = components["schemas"]["WrittenMigration"];

export const draftKeys = {
  list: (org: string, db: string) => ["drafts", org, db] as const,
  one: (org: string, db: string, id: string) => ["drafts", org, db, id] as const,
  preview: (org: string, db: string, id: string) => ["drafts", org, db, id, "publish"] as const,
};

/** The open drafts of a database, with who is in each. Polled: presence moves. */
export const draftsQuery = (org: string, db: string) =>
  queryOptions({
    queryKey: draftKeys.list(org, db),
    queryFn: () =>
      unwrap(api.GET("/api/orgs/{org}/databases/{db}/drafts", { params: { path: { org, db } } })),
    refetchInterval: 15_000,
  });

export const draftQuery = (org: string, db: string, id: string) =>
  queryOptions({
    queryKey: draftKeys.one(org, db, id),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/drafts/{id}", {
          params: { path: { org, db, id } },
        }),
      ),
  });

/** What publishing would create now: always fresh, the draft moves under it. */
export const publishPreviewQuery = (org: string, db: string, id: string) =>
  queryOptions({
    queryKey: draftKeys.preview(org, db, id),
    queryFn: () =>
      unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/drafts/{id}/publish", {
          params: { path: { org, db, id } },
        }),
      ),
    staleTime: 0,
    gcTime: 0,
  });
