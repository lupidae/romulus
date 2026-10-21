import type { AccessTokenScope } from "./queries";

export const scopeLabel: Record<AccessTokenScope, string> = {
  write: "Read and write",
  read: "Read only",
};

export const scopeDescription: Record<AccessTokenScope, string> = {
  write: "Everything your role allows: drafts, migrations, tests, publishing.",
  read: "Reads schemas, drafts and tests, queries playgrounds. Never changes anything.",
};

/** Where people read how it works: the docs in the repository. */
export const DOCS_URL = "https://github.com/lupidae/romulus/blob/main/docs/mcp.md";
