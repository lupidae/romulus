import { useQuery } from "@tanstack/react-query";
import { createFileRoute, redirect } from "@tanstack/react-router";
import * as React from "react";

import { healthQuery } from "@/api/health";
import { LogoMark } from "@/components/brand/logo";
import { LockIcon } from "@/components/icons";
import { SchemaPreview } from "@/components/shell/schema-preview";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { authMethodsQuery, meQuery } from "@/features/auth/queries";
import { devSigninUrl, loginUrl } from "@/features/auth/session";
import { safePath } from "@/lib/safe-path";

interface SigninSearch {
  /**
   * Where to land after signing in: a same-origin path including the /app
   * prefix (the API redirects there after the callback). Anything else is
   * dropped here, and the API checks it again.
   */
  next?: string;
  /** Set by the API's OIDC callback (or the app) when sign-in did not complete. */
  error?: string;
}

// Messages for the codes the auth callback sends back. Unknown codes fall back
// to a generic line: the page never shows a raw identifier.
const errors: Record<string, { title: string; body: string }> = {
  access_denied: {
    title: "Sign-in was cancelled",
    body: "Nothing was changed. Try again when you are ready.",
  },
  session_expired: {
    title: "Your session expired",
    body: "Sign in again to pick up where you left off.",
  },
  email_unverified: {
    title: "Verify your email first",
    body: "romulus needs a verified email address. Verify it with your identity provider, then sign in again.",
  },
  sign_in_expired: {
    title: "That sign-in took too long",
    body: "Sign-ins are valid for 10 minutes and work once. Start again.",
  },
  sso_unavailable: {
    title: "Single sign-on is not answering",
    body: "romulus could not reach your identity provider. Try again in a moment.",
  },
};
const fallbackError = {
  title: "Sign-in did not complete",
  body: "Something went wrong between romulus and your identity provider. Try again.",
};

// The test users the Zitadel provisioner creates (CLAUDE.md, "Run it").
const devUsers = ["alice@romulus.test", "bob@romulus.test"];

export const Route = createFileRoute("/signin")({
  validateSearch: (search: Record<string, unknown>): SigninSearch => ({
    next: safePath(search.next),
    error: typeof search.error === "string" ? search.error : undefined,
  }),
  // Already signed in: straight on to where the link was going.
  beforeLoad: async ({ context, search }) => {
    const me = await context.queryClient.ensureQueryData(meQuery).catch(() => undefined);
    if (me) {
      const next = search.next?.startsWith("/app/") ? search.next.slice(4) : "/";
      throw redirect({ href: next, replace: true });
    }
  },
  head: () => ({ meta: [{ title: "Sign in – romulus" }] }),
  component: SignIn,
});

function SignIn() {
  const { next, error } = Route.useSearch();
  const health = useQuery(healthQuery);
  const methods = useQuery(authMethodsQuery);
  const failure = error ? (errors[error] ?? fallbackError) : undefined;
  const ssoMissing = methods.data?.sso === false;
  const ssoBlocked = health.isError || ssoMissing;

  return (
    <div className="grid min-h-dvh bg-background lg:grid-cols-[minmax(0,1fr)_minmax(0,1.1fr)]">
      <main className="flex flex-col px-6 py-6 sm:px-10">
        <div className="flex flex-1 items-center justify-center pb-10">
          <div className="w-full max-w-sm">
            <LogoMark className="size-10" />
            <h1 className="mt-8 text-2xl font-semibold text-foreground">Sign in to romulus</h1>
            <p className="mt-2 text-base text-muted-foreground">
              Version, review and design your Postgres schemas together.
            </p>

            <div className="mt-8 grid gap-3">
              {failure && (
                <Alert variant="destructive" title={failure.title}>
                  {failure.body}
                </Alert>
              )}
              {health.isError && (
                <Alert
                  variant="warning"
                  title="romulus is not answering"
                  action={
                    <Button
                      size="sm"
                      loading={health.isFetching}
                      onClick={() => void health.refetch()}
                    >
                      Retry
                    </Button>
                  }
                >
                  Sign-in is paused until the server is back.
                </Alert>
              )}
              {ssoMissing && !health.isError && (
                <Alert variant="warning" title="Single sign-on is not set up">
                  This server has no identity provider configured yet.
                </Alert>
              )}
              {ssoBlocked ? (
                <Button variant="primary" size="lg" className="w-full" disabled>
                  <LockIcon />
                  Continue with SSO
                </Button>
              ) : (
                <Button variant="primary" size="lg" className="w-full" asChild>
                  <a href={loginUrl(next)}>
                    <LockIcon />
                    Continue with SSO
                  </a>
                </Button>
              )}
            </div>

            <p className="mt-4 text-sm text-muted-foreground">
              You sign in with your company account, then come straight back here.
            </p>

            {methods.data?.dev_signin && <DevSignin next={next} />}
          </div>
        </div>
      </main>

      <aside
        aria-hidden
        className="relative hidden overflow-hidden border-l border-border bg-muted lg:block"
      >
        <SchemaPreview />
      </aside>
    </div>
  );
}

/**
 * Local stacks only (`ROMULUS_DEV_SIGNIN`): sign in as anyone without the
 * identity provider, for QA and end-to-end tests.
 */
function DevSignin({ next }: { next?: string }) {
  const [email, setEmail] = React.useState("");
  return (
    <section aria-labelledby="dev-signin" className="mt-8 grid gap-3">
      <div className="flex items-center gap-3">
        <Separator className="flex-1" />
        <h2 id="dev-signin" className="text-xs font-medium text-faint-foreground">
          Dev sign-in, local stacks only
        </h2>
        <Separator className="flex-1" />
      </div>
      <div className="grid grid-cols-2 gap-2">
        {devUsers.map((user) => (
          <Button key={user} asChild>
            <a href={devSigninUrl(user, next)}>{user.split("@")[0]}</a>
          </Button>
        ))}
      </div>
      <form
        className="flex gap-2"
        onSubmit={(event) => {
          event.preventDefault();
          if (email.trim()) window.location.assign(devSigninUrl(email.trim(), next));
        }}
      >
        <Input
          type="email"
          aria-label="Sign in as any email"
          placeholder="anyone@romulus.test"
          autoComplete="off"
          spellCheck={false}
          value={email}
          onChange={(e) => setEmail(e.target.value)}
        />
        <Button type="submit">Sign in</Button>
      </form>
    </section>
  );
}
