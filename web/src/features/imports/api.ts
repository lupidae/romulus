import { ApiError, api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

import type { PickedFile } from "./files";

export type ImportPreview = components["schemas"]["ImportPreview"];
export type ImportReport = components["schemas"]["ImportReport"];
export type ImportEvent = components["schemas"]["ImportEvent"];
export type ImportFailure = components["schemas"]["ImportFailure"];
export type ImportError = components["schemas"]["ImportError"];
export type PlannedMigration = components["schemas"]["PlannedMigration"];
export type MigrationFormat = components["schemas"]["MigrationFormat"];
export type MigrationStatus = components["schemas"]["MigrationStatus"];
export type SkippedStatement = components["schemas"]["SkippedStatement"];
export type SkipReason = components["schemas"]["SkipReason"];
export type IgnoreReason = components["schemas"]["IgnoreReason"];

export interface ImportOptions {
  /** Read the files as this tool's (detected when absent). */
  format?: MigrationFormat;
  /** Add the imported versions after versions that were not imported. */
  append?: boolean;
}

/** The files as a multipart form, each named by its path. */
function form(files: PickedFile[]): FormData {
  const data = new FormData();
  for (const { path, file } of files) data.append("files", file, path);
  return data;
}

const asIs = (body: unknown) => body as BodyInit;

/** What importing the files would do: format, order, what each skips. */
export function previewImport(
  org: string,
  db: string,
  files: PickedFile[],
  options: ImportOptions,
): Promise<ImportPreview> {
  return unwrap(
    api.POST("/api/orgs/{org}/databases/{db}/imports/preview", {
      params: { path: { org, db }, query: { format: options.format } },
      body: form(files) as never,
      bodySerializer: asIs,
    }),
  );
}

/**
 * Imports the files, calling `onEvent` for each line of progress. Resolves
 * with the report; rejects with an `ApiError` when the import is refused
 * (the HTTP status) or stops (the stream's `failed` event).
 */
export async function runImport(
  org: string,
  db: string,
  files: PickedFile[],
  options: ImportOptions,
  onEvent: (event: ImportEvent) => void,
): Promise<ImportReport> {
  const { data, error, response } = await api.POST("/api/orgs/{org}/databases/{db}/imports", {
    params: {
      path: { org, db },
      query: { format: options.format, append: options.append || undefined },
    },
    body: form(files) as never,
    bodySerializer: asIs,
    headers: { Accept: "application/x-ndjson" },
    parseAs: "stream",
  });
  if (!response.ok || !data) {
    throw new ApiError(response.status, error);
  }
  let report: ImportReport | undefined;
  for await (const event of lines(data)) {
    onEvent(event);
    if (event.event === "done") report = event.report;
    if (event.event === "failed") {
      throw new ApiError(statusOf(event.error.code), event.error);
    }
  }
  if (!report)
    throw new ApiError(502, {
      code: "unavailable",
      message: "The import stopped without a result. Check the history, then try again.",
    });
  return report;
}

/** The status a failure would have had as an ordinary answer. */
function statusOf(code: string): number {
  if (
    ["has_versions", "migration_changed", "migration_out_of_order", "concurrent_import"].includes(
      code,
    )
  )
    return 409;
  if (code === "unavailable" || code === "timeout") return 503;
  if (code === "internal") return 500;
  return 422;
}

/** NDJSON: one event per line, as they arrive. */
export async function* lines(stream: ReadableStream<Uint8Array>): AsyncGenerator<ImportEvent> {
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
      if (line) yield JSON.parse(line) as ImportEvent;
      newline = buffer.indexOf("\n");
    }
    if (done) break;
  }
  if (buffer.trim()) yield JSON.parse(buffer) as ImportEvent;
}

/** The failure a 422 `import_failed` carries in `details`. */
export function failureOf(error: unknown): ImportFailure | null {
  if (!(error instanceof ApiError) || error.code !== "import_failed") return null;
  const details = error.details as Partial<ImportFailure> | undefined;
  return details && typeof details.index === "number" ? (details as ImportFailure) : null;
}
