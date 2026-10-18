import { useMutation, useQueryClient } from "@tanstack/react-query";

import { ApiError, api, succeed, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

import {
  type MigrationEvent,
  type MigrationReport,
  type Playground,
  playgroundKeys,
} from "./queries";

export type CreatePlayground = components["schemas"]["CreatePlayground"];

export function useCreatePlayground(org: string, db: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: CreatePlayground) =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/playgrounds", {
          params: { path: { org, db } },
          body,
        }),
      ),
    onSuccess: (playground) => {
      queryClient.setQueryData(playgroundKeys.one(org, db, playground.id), playground);
      void queryClient.invalidateQueries({ queryKey: playgroundKeys.list(org, db), exact: true });
    },
  });
}

export function useDeletePlayground(org: string, db: string, id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      succeed(
        api.DELETE("/api/orgs/{org}/databases/{db}/playgrounds/{id}", {
          params: { path: { org, db, id } },
        }),
      ),
    onSuccess: () => {
      queryClient.removeQueries({ queryKey: playgroundKeys.one(org, db, id) });
      void queryClient.invalidateQueries({ queryKey: playgroundKeys.list(org, db), exact: true });
    },
  });
}

export function useResetPlayground(org: string, db: string, id: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      unwrap(
        api.POST("/api/orgs/{org}/databases/{db}/playgrounds/{id}/reset", {
          params: { path: { org, db, id } },
        }),
      ),
    onSuccess: (playground) => {
      queryClient.setQueryData<Playground>(playgroundKeys.one(org, db, id), playground);
      void queryClient.invalidateQueries({ queryKey: playgroundKeys.runs(org, db, id) });
    },
  });
}

/**
 * Runs the draft's migration on the playground's rows (reset first),
 * reading its progress as it comes. Resolves with the report; a run that
 * could not happen throws an ApiError.
 */
export async function runMigration(
  org: string,
  db: string,
  id: string,
  sha256: string,
  onEvent: (event: MigrationEvent) => void,
): Promise<MigrationReport> {
  const { data, error, response } = await api.POST(
    "/api/orgs/{org}/databases/{db}/playgrounds/{id}/migration",
    {
      params: { path: { org, db, id } },
      body: { sha256 },
      parseAs: "stream",
    },
  );
  if (!response.ok || !data) throw new ApiError(response.status, error);
  let report: MigrationReport | undefined;
  for await (const event of lines(data)) {
    onEvent(event);
    if (event.type === "done") report = event.report;
    if (event.type === "failed")
      throw new ApiError(503, { code: "host_unavailable", message: event.message });
  }
  if (!report)
    throw new ApiError(502, {
      code: "unavailable",
      message: "The run stopped without a report. Its entry in the history says how it ended.",
    });
  return report;
}

/** NDJSON: one event per line, as they arrive; blank lines keep it alive. */
async function* lines(stream: ReadableStream<Uint8Array>): AsyncGenerator<MigrationEvent> {
  const decoder = new TextDecoderStream() as unknown as ReadableWritablePair<string, Uint8Array>;
  const reader = stream.pipeThrough(decoder).getReader();
  let buffer = "";
  for (;;) {
    const { value, done } = await reader.read();
    if (value) buffer += value;
    let newline = buffer.indexOf("\n");
    while (newline >= 0) {
      const line = buffer.slice(0, newline).trim();
      buffer = buffer.slice(newline + 1);
      if (line) yield JSON.parse(line) as MigrationEvent;
      newline = buffer.indexOf("\n");
    }
    if (done) break;
  }
  if (buffer.trim()) yield JSON.parse(buffer) as MigrationEvent;
}
