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
