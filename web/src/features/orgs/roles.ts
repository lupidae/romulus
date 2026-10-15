import type { Role } from "./queries";

/** Most powerful first, as the API's matrix (docs/MVP.md "Roles"). */
export const roles: readonly Role[] = ["admin", "developer", "reviewer"];

export const roleLabel: Record<Role, string> = {
  admin: "Admin",
  developer: "Developer",
  reviewer: "Reviewer",
};

/** One line each, what the role may do: shown wherever a role is picked. */
export const roleDescription: Record<Role, string> = {
  admin: "Everything, plus members and settings",
  developer: "Edits drafts, creates versions and databases",
  reviewer: "Reads everything and follows drafts live, edits nothing",
};

/** What joining with the role lets you do, as a sentence for the invitee. */
export const roleAbilities: Record<Role, string> = {
  admin: "As an admin you can do everything, including managing members and settings.",
  developer: "As a developer you can edit drafts and create versions and databases.",
  reviewer: "As a reviewer you can read everything and follow drafts live, without editing.",
};

/** Mirrors `Role::can_manage_members`: invite, change roles, remove, settings. */
export const canManageMembers = (role: Role) => role === "admin";

/** Mirrors `Role::can_write`: drafts, versions, uploads, tokens, databases. */
export const canWrite = (role: Role) => role !== "reviewer";

/** "an admin", "a developer"… */
export function withArticle(role: Role): string {
  return `${role === "admin" ? "an" : "a"} ${role}`;
}
