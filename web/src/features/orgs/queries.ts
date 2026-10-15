import { queryOptions } from "@tanstack/react-query";

import { api, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

export type Org = components["schemas"]["Org"];
export type Role = components["schemas"]["Role"];
export type OrgMember = components["schemas"]["OrgMember"];
export type Invitation = components["schemas"]["Invitation"];
export type CreatedInvitation = components["schemas"]["CreatedInvitation"];
export type InvitationPreview = components["schemas"]["InvitationPreview"];

export const orgKeys = {
  all: ["orgs"] as const,
  one: (slug: string) => ["orgs", slug] as const,
  members: (slug: string) => ["orgs", slug, "members"] as const,
  invitations: (slug: string) => ["orgs", slug, "invitations"] as const,
};

/** The organizations I belong to, with my role in each. */
export const orgsQuery = queryOptions({
  queryKey: orgKeys.all,
  queryFn: () => unwrap(api.GET("/api/orgs")),
});

export const orgQuery = (slug: string) =>
  queryOptions({
    queryKey: orgKeys.one(slug),
    queryFn: () => unwrap(api.GET("/api/orgs/{org}", { params: { path: { org: slug } } })),
  });

export const membersQuery = (slug: string) =>
  queryOptions({
    queryKey: orgKeys.members(slug),
    queryFn: () => unwrap(api.GET("/api/orgs/{org}/members", { params: { path: { org: slug } } })),
  });

/** Pending invitations (admins only). */
export const invitationsQuery = (slug: string) =>
  queryOptions({
    queryKey: orgKeys.invitations(slug),
    queryFn: () =>
      unwrap(api.GET("/api/orgs/{org}/invitations", { params: { path: { org: slug } } })),
  });

/** What an invitation link invites to, judged for whoever looks (signed in or not). */
export const invitationPreviewQuery = (token: string) =>
  queryOptions({
    queryKey: ["invitations", token],
    queryFn: () => unwrap(api.GET("/api/invitations/{token}", { params: { path: { token } } })),
    staleTime: 0,
  });
