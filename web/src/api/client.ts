import createClient, { type Middleware } from "openapi-fetch";

import type { components, paths } from "./schema.gen";

/** The API's error body: a stable `code` to branch on, a `message` to show. */
export type ErrorBody = components["schemas"]["ErrorBody"];

/** A non-2xx answer from the API, thrown by `unwrap` so TanStack Query sees an error. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;

  constructor(status: number, body: Partial<ErrorBody> | undefined) {
    super(body?.message ?? `Request failed (${status})`);
    this.name = "ApiError";
    this.status = status;
    this.code = body?.code ?? "unknown";
  }
}

// The API refuses cookie-authenticated mutations without this header (CSRF);
// a cross-site form cannot set it. See crates/romulus-api/src/csrf.rs.
const csrf: Middleware = {
  onRequest({ request }) {
    if (!["GET", "HEAD", "OPTIONS"].includes(request.method)) {
      request.headers.set("X-Romulus-Csrf", "1");
    }
    return request;
  },
};

/**
 * The typed API client. Paths, parameters and bodies come from the Rust
 * OpenAPI document (`just gen`); never declare an API type by hand.
 *
 *   const health = await unwrap(api.GET("/api/health"));
 */
export const api = createClient<paths>({
  baseUrl: typeof window === "undefined" ? "http://localhost" : window.location.origin,
  credentials: "same-origin",
  // Looked up per call, not captured at import, so tests and mocks can stub it.
  fetch: (request) => globalThis.fetch(request),
});
api.use(csrf);

/** Returns the data of an openapi-fetch call, or throws an ApiError. */
export async function unwrap<T>(
  call: Promise<{ data?: T; error?: unknown; response: Response }>,
): Promise<T> {
  const { data, error, response } = await call;
  if (!response.ok || data === undefined) {
    throw new ApiError(response.status, error as Partial<ErrorBody> | undefined);
  }
  return data;
}
