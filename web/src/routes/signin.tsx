import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";

import { healthQuery } from "@/api/health";
import { LogoMark } from "@/components/brand/logo";
import { LockIcon } from "@/components/icons";
import { SchemaPreview } from "@/components/shell/schema-preview";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";

interface SigninSearch {
  /**
   * Where to land after signing in: a same-origin path including the /app
   * prefix (the API redirects there after the callback). Anything else is
   * dropped here, and the API must check it again.
   */
  next?: string;
  /** Set by the API's OIDC callback when sign-in did not complete. */
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
    body: "romulus needs a verified email address. Check your inbox, then sign in again.",
  },
};
const fallbackError = {
  title: "Sign-in did not complete",
  body: "Something went wrong between romulus and your identity provider. Try again.",
};

function safePath(value: unknown): string | undefined {
  return typeof value === "string" && value.startsWith("/") && !value.startsWith("//")
    ? value
    : undefined;
}

export const Route = createFileRoute("/signin")({
  validateSearch: (search: Record<string, unknown>): SigninSearch => ({
    next: safePath(search.next),
    error: typeof search.error === "string" ? search.error : undefined,
  }),
  head: () => ({ meta: [{ title: "Sign in – romulus" }] }),
  component: SignIn,
});

function SignIn() {
  // Seam for the auth module: with ROMULUS_DEV_SIGNIN on, this page also
  // offers the test users (see crates/romulus-api/src/auth/mod.rs).
  const { next, error } = Route.useSearch();
  const health = useQuery(healthQuery);
  const loginUrl = `/api/auth/login${next ? `?next=${encodeURIComponent(next)}` : ""}`;
  const failure = error ? (errors[error] ?? fallbackError) : undefined;

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
              {health.isError ? (
                <Button variant="primary" size="lg" className="w-full" disabled>
                  <LockIcon />
                  Continue with SSO
                </Button>
              ) : (
                <Button variant="primary" size="lg" className="w-full" asChild>
                  <a href={loginUrl}>
                    <LockIcon />
                    Continue with SSO
                  </a>
                </Button>
              )}
            </div>

            <p className="mt-4 text-sm text-muted-foreground">
              You sign in with your company account, then come straight back here.
            </p>
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
