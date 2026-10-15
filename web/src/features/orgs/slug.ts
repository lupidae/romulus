/** Top-level app routes an organization slug would shadow (the API refuses them too). */
export const RESERVED_SLUGS = ["signin", "new", "invite", "dev", "settings", "api", "app"];

export const SLUG_MAX = 63;

/** A URL-safe slug from a name: "Acme Inc." → "acme-inc", "Café Ünïon" → "cafe-union". */
export function slugify(name: string): string {
  return name
    .normalize("NFKD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, SLUG_MAX)
    .replace(/-+$/g, "");
}

/** Why `slug` cannot be an organization's address, or undefined. Same rules as the API. */
export function slugProblem(slug: string): string | undefined {
  if (!slug) return "Choose a URL for the organization.";
  if (slug.length > SLUG_MAX) return "Keep the URL to 63 characters or fewer.";
  if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(slug)) {
    return "Use lowercase letters, digits and single hyphens, not at the start or end.";
  }
  if (RESERVED_SLUGS.includes(slug)) return "This URL is reserved by romulus. Pick another one.";
  return undefined;
}
