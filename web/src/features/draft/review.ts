// Approvals before publishing (docs/MVP.md "Review round 1"), as the
// collab server tells them (its `review` event): each approval is of one
// text, so an edit resets them; whoever edited the SQL cannot approve.

import type { components } from "@/api/schema.gen";
import type { Role } from "@/features/orgs/queries";

import type { ServerEvent } from "./queries";

export type Review = Omit<Extract<ServerEvent, { type: "review" }>, "type">;
export type ReviewApproval = components["schemas"]["ReviewApproval"];

/** Approvals are asked for at all (the organization's setting). */
export function reviewing(review: Review | null): review is Review {
  return review !== null && review.required > 0;
}

/** Enough approvals that count. */
export function approved(review: Review): boolean {
  return review.approvals.length >= review.required;
}

/** What this person may do about the review. */
export function standing(
  review: Review,
  meId: string,
  role: Role | null,
): "approved" | "can_approve" | "editor" | "none" {
  if (review.editors.includes(meId)) return "editor";
  if (review.approvals.some((a) => a.user_id === meId)) return "approved";
  return role ? "can_approve" : "none";
}

/** "1 of 2 approvals", "2 of 2 approvals". */
export function approvalsLabel(review: Review): string {
  return `${Math.min(review.approvals.length, review.required)} of ${review.required} approval${review.required === 1 ? "" : "s"}`;
}

/**
 * The approvals an edit reset between two states: counted before, of
 * another text now. (A withdrawal or a new editor is not a reset.)
 */
export function resetApprovals(before: Review | null, after: Review): ReviewApproval[] {
  if (!before) return [];
  return before.approvals.filter((a) =>
    after.outdated.some((o) => o.user_id === a.user_id && o.at === a.at),
  );
}

/** "Bob", "Bob and Carol", "Bob, Carol and Dan". */
export function names(people: { name: string }[]): string {
  const list = people.map((p) => p.name);
  if (list.length <= 1) return list.join("");
  return `${list.slice(0, -1).join(", ")} and ${list.at(-1)}`;
}

/** SHA-256 of the SQL in hex: what an approval is of. */
export async function sha256Hex(text: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(text));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}
