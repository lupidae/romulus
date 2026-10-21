// The exports (terms 17.1): everything about a database, or an organization,
// as a ZIP the API streams. Same origin, the cookie goes along.

import { ApiError, type ErrorBody } from "@/api/client";
import { save } from "@/features/migration/zip";

/** Where a database's export downloads from (any member). */
export function databaseExportUrl(org: string, db: string): string {
  return `/api/orgs/${encodeURIComponent(org)}/databases/${encodeURIComponent(db)}/export`;
}

/** Where a whole organization's export downloads from (admins). */
export function orgExportUrl(org: string): string {
  return `/api/orgs/${encodeURIComponent(org)}/export`;
}

/**
 * Downloads the export at `url` and saves it under the name the API gives.
 * Fetched rather than linked so a refusal (429 `rate_limited`…) is thrown as
 * an `ApiError`, never saved as the file.
 */
export async function downloadExport(url: string): Promise<void> {
  const response = await fetch(url, { credentials: "same-origin" });
  if (!response.ok) throw await refusal(response);
  save(fileName(response.headers.get("Content-Disposition")), await response.blob());
}

async function refusal(response: Response): Promise<ApiError> {
  const body = (await response.json().catch(() => undefined)) as Partial<ErrorBody> | undefined;
  // A proxy's 429 may carry only the header: `retryAfter` reads the details.
  const wait = Number(response.headers.get("Retry-After"));
  if (response.status === 429 && body?.details === undefined && wait > 0) {
    return new ApiError(429, { ...body, details: { retry_after: wait } });
  }
  return new ApiError(response.status, body);
}

/** `attachment; filename="acme-export-2026-10-21.zip"` → the name. */
function fileName(disposition: string | null): string {
  return disposition?.match(/filename="([^"]+)"/)?.[1] ?? "export.zip";
}
