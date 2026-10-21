// What the shell displays. Route code maps API responses onto these, so the
// shell never depends on the shape of an endpoint.
export interface ShellUser {
  id: string;
  name: string;
  email: string;
}

export interface ShellOrg {
  id: string;
  slug: string;
  name: string;
  /** Shown next to the org in the switcher. */
  role: "admin" | "developer" | "reviewer";
}

/** Which romulus runs, and where its source is (AGPL-3.0, section 13). */
export interface ShellSource {
  /** The source at the commit that runs, or the repository when unknown. */
  url: string;
  commit?: string | null;
  version: string;
}
