import { useMutation, useQueryClient } from "@tanstack/react-query";

import { api, succeed, unwrap } from "@/api/client";
import type { components } from "@/api/schema.gen";

import { type Invitation, type Org, orgKeys, type OrgMember, type Role } from "./queries";

type CreateOrg = components["schemas"]["CreateOrg"];
type UpdateOrg = components["schemas"]["UpdateOrg"];

export function useCreateOrg() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: CreateOrg) => unwrap(api.POST("/api/orgs", { body })),
    onSuccess: (org) => {
      queryClient.setQueryData(orgKeys.one(org.slug), org);
      queryClient.setQueryData<Org[]>(orgKeys.all, (orgs) =>
        orgs ? [...orgs, org].sort((a, b) => a.name.localeCompare(b.name)) : orgs,
      );
      void queryClient.invalidateQueries({ queryKey: orgKeys.all, exact: true });
    },
  });
}

export function useUpdateOrg(slug: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: UpdateOrg) =>
      unwrap(api.PATCH("/api/orgs/{org}", { params: { path: { org: slug } }, body })),
    onSuccess: (org) => {
      queryClient.setQueryData(orgKeys.one(org.slug), org);
      void queryClient.invalidateQueries({ queryKey: orgKeys.all, exact: true });
    },
  });
}

/** Changes a role at once in the list, and puts it back if the API refuses. */
export function useChangeRole(slug: string, myId: string) {
  const queryClient = useQueryClient();
  const key = orgKeys.members(slug);
  return useMutation({
    mutationFn: ({ userId, role }: { userId: string; role: Role }) =>
      unwrap(
        api.PATCH("/api/orgs/{org}/members/{user}", {
          params: { path: { org: slug, user: userId } },
          body: { role },
        }),
      ),
    onMutate: async ({ userId, role }) => {
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<OrgMember[]>(key);
      queryClient.setQueryData<OrgMember[]>(key, (members) =>
        members?.map((m) => (m.user_id === userId ? { ...m, role } : m)),
      );
      return { previous };
    },
    onError: (_error, _vars, context) => queryClient.setQueryData(key, context?.previous),
    onSettled: (_data, _error, { userId }) => {
      void queryClient.invalidateQueries({ queryKey: key });
      // My own role changed: every screen's permissions follow.
      if (userId === myId) {
        void queryClient.invalidateQueries({ queryKey: orgKeys.one(slug), exact: true });
        void queryClient.invalidateQueries({ queryKey: orgKeys.all, exact: true });
      }
    },
  });
}

export function useRemoveMember(slug: string) {
  const queryClient = useQueryClient();
  const key = orgKeys.members(slug);
  return useMutation({
    mutationFn: (userId: string) =>
      succeed(
        api.DELETE("/api/orgs/{org}/members/{user}", {
          params: { path: { org: slug, user: userId } },
        }),
      ),
    onSuccess: (_ok, userId) => {
      queryClient.setQueryData<OrgMember[]>(key, (members) =>
        members?.filter((m) => m.user_id !== userId),
      );
      void queryClient.invalidateQueries({ queryKey: key });
    },
  });
}

export function useInvite(slug: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: (body: { email: string; role: Role }) =>
      unwrap(api.POST("/api/orgs/{org}/invitations", { params: { path: { org: slug } }, body })),
    onSuccess: ({ invitation }) => {
      queryClient.setQueryData<Invitation[]>(orgKeys.invitations(slug), (pending) =>
        pending
          ? [
              invitation,
              ...pending.filter((i) => i.email.toLowerCase() !== invitation.email.toLowerCase()),
            ]
          : pending,
      );
      void queryClient.invalidateQueries({ queryKey: orgKeys.invitations(slug) });
    },
  });
}

export function useRevokeInvitation(slug: string) {
  const queryClient = useQueryClient();
  const key = orgKeys.invitations(slug);
  return useMutation({
    mutationFn: (id: string) =>
      succeed(
        api.DELETE("/api/orgs/{org}/invitations/{id}", { params: { path: { org: slug, id } } }),
      ),
    onSuccess: (_ok, id) => {
      queryClient.setQueryData<Invitation[]>(key, (pending) => pending?.filter((i) => i.id !== id));
      void queryClient.invalidateQueries({ queryKey: key });
    },
  });
}

export function useAcceptInvitation(token: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      unwrap(api.POST("/api/invitations/{token}/accept", { params: { path: { token } } })),
    onSuccess: ({ org }) => {
      queryClient.setQueryData(orgKeys.one(org.slug), org);
      void queryClient.invalidateQueries({ queryKey: orgKeys.all, exact: true });
    },
  });
}

/** Leaves the organization: it disappears from my list at once. */
export function useLeaveOrg(slug: string, myId: string) {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: () =>
      succeed(
        api.DELETE("/api/orgs/{org}/members/{user}", {
          params: { path: { org: slug, user: myId } },
        }),
      ),
    onSuccess: () => {
      queryClient.setQueryData<Org[]>(orgKeys.all, (orgs) => orgs?.filter((o) => o.slug !== slug));
      queryClient.removeQueries({ queryKey: orgKeys.one(slug) });
      void queryClient.invalidateQueries({ queryKey: orgKeys.all, exact: true });
    },
  });
}
