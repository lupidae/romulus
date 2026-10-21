// The exports (terms 17.1): everything about a database, or an organization,
// as a ZIP the API streams. Plain links: same origin, the cookie goes along.

/** Where a database's export downloads from (any member). */
export function databaseExportUrl(org: string, db: string): string {
  return `/api/orgs/${encodeURIComponent(org)}/databases/${encodeURIComponent(db)}/export`;
}

/** Where a whole organization's export downloads from (admins). */
export function orgExportUrl(org: string): string {
  return `/api/orgs/${encodeURIComponent(org)}/export`;
}
