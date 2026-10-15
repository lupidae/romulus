// The organization to open from /app/: the last one visited, per browser.
// A convenience only: storage can be unavailable (private mode), and a stale
// slug is ignored by the caller when it is not one of the user's orgs.
const KEY = "romulus-last-org";

export function readLastOrg(): string | undefined {
  try {
    return localStorage.getItem(KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

export function rememberLastOrg(slug: string) {
  try {
    localStorage.setItem(KEY, slug);
  } catch {
    // Nothing to do: the first organization opens instead.
  }
}
