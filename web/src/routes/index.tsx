import { createFileRoute, redirect } from "@tanstack/react-router";

import { requireUser } from "@/features/auth/session";
import { readLastOrg } from "@/features/orgs/last-org";
import { orgsQuery } from "@/features/orgs/queries";

export const Route = createFileRoute("/")({
  // Signed out → sign-in. Signed in → the last organization visited (or the
  // first), or "create an organization" for someone in none yet.
  beforeLoad: async ({ context: { queryClient }, location }) => {
    await requireUser(queryClient, location.href);
    const orgs = await queryClient.ensureQueryData(orgsQuery);
    const last = readLastOrg();
    const org = orgs.find((o) => o.slug === last) ?? orgs[0];
    if (!org) throw redirect({ to: "/new", replace: true });
    throw redirect({ to: "/$org", params: { org: org.slug }, replace: true });
  },
});
