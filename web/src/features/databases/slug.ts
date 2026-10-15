import { SLUG_MAX } from "@/features/orgs/slug";

export { slugify } from "@/features/orgs/slug";

/** The organization's own pages a database slug would shadow (the API refuses them too). */
export const RESERVED_DATABASE_SLUGS = [
  "settings",
  "new",
  "members",
  "invitations",
  "search",
  "activity",
];

/** Why `slug` cannot be a database's address, or undefined. Same rules as the API. */
export function databaseSlugProblem(slug: string): string | undefined {
  if (!slug) return "Choose a URL for the database.";
  if (slug.length > SLUG_MAX) return "Keep the URL to 63 characters or fewer.";
  if (!/^[a-z0-9]+(-[a-z0-9]+)*$/.test(slug)) {
    return "Use lowercase letters, digits and single hyphens, not at the start or end.";
  }
  if (RESERVED_DATABASE_SLUGS.includes(slug)) {
    return "This URL is used by the organization's own pages. Pick another one.";
  }
  return undefined;
}
