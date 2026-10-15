import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/")({
  // Seam for the auth module: there are no sessions in S0, so everyone signs
  // in. Once /api/auth/me exists: signed in → the last organization (or the
  // "create an organization" page when there is none), signed out → /signin.
  beforeLoad: () => {
    throw redirect({ to: "/signin" });
  },
});
