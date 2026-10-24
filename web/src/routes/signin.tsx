import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link, redirect } from "@tanstack/react-router";
import * as React from "react";

import { healthQuery } from "@/api/health";
import { LockIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { AuthPage, authLinkClass } from "@/features/auth/components/auth-page";
import { PasswordSignInForm } from "@/features/auth/components/password-forms";
import { authMethodsQuery, meQuery } from "@/features/auth/queries";
import { afterSignIn, devSigninUrl, loginUrl } from "@/features/auth/session";
import { nextWithSource } from "@/features/marketing/source";
import { afterSignInLine, subscribeInNext } from "@/features/billing/subscribe";
import { safePath } from "@/lib/safe-path";

interface SigninSearch {
  /** The address to sign in with, pre-filled (an invitation's). */
  email?: string;
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
interface SigninMessage {
  title: string;
  body: string;
  /** Not a failure (an expired session, a cancel): shown as information. */
  neutral?: boolean;
}

const errors: Record<string, SigninMessage> = {
  access_denied: {
    title: "Sign-in was cancelled",
    body: "Nothing was changed. Try again when you are ready.",
    neutral: true,
  },
  session_expired: {
    title: "Your session expired",
    body: "Sign in again to pick up where you left off.",
    neutral: true,
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
  too_many_sign_ins: {
    title: "Too many sign-in attempts",
    body: "Wait a minute, then try again.",
  },
};
const fallbackError: SigninMessage = {
  title: "Sign-in did not complete",
  body: "Something went wrong between romulus and your identity provider. Try again.",
};

// The test users the Zitadel provisioner creates (CLAUDE.md, "Run it").
const devUsers = ["alice@romulus.test", "bob@romulus.test"];

export const Route = createFileRoute("/signin")({
  validateSearch: (search: Record<string, unknown>): SigninSearch => ({
    next: safePath(search.next),
    error: typeof search.error === "string" ? search.error : undefined,
    email: typeof search.email === "string" ? search.email.slice(0, 254) : undefined,
  }),
  // Already signed in: straight on to where the link was going.
  beforeLoad: async ({ context, search }) => {
    // Asked again, never from the cache: the page also receives people whose
    // session just ended.
    const me = await context.queryClient
      .fetchQuery({ ...meQuery, staleTime: 0 })
      .catch(() => undefined);
    if (me) throw redirect({ href: afterSignIn(search.next), replace: true });
  },
  head: () => ({ meta: [{ title: "Sign in – romulus" }] }),
  component: SignIn,
});

function SignIn() {
  const { next: asked, error, email } = Route.useSearch();
  // Where to land, with this visit's campaign parameters and referrer, so
  // the page signing in reports them (features/marketing/source.ts).
  const next = nextWithSource(asked);
  const health = useQuery(healthQuery);
  const methods = useQuery(authMethodsQuery);
  const failure = error ? (errors[error] ?? fallbackError) : undefined;
  // From the homepage's plan buttons (/app/signup): what follows.
  const subscribing = subscribeInNext(asked);
  const password = methods.data?.password === true;
  // Single sign-on is offered when it works, or when it is the only way in
  // (then disabled, saying why).
  const sso = methods.data !== undefined && (methods.data.sso || !password);
  const ssoMissing = methods.data !== undefined && !methods.data.sso && !password;
  const ssoBlocked = health.isError || ssoMissing;

  return (
    <AuthPage
      title="Sign in to romulus"
      description={
        subscribing
          ? `Sign in, or create your account. ${afterSignInLine(subscribing)}`
          : "Version, review and design your Postgres schemas together."
      }
      footer={
        password &&
        (methods.data?.sign_up === "open" ? (
          <>
            New to romulus?{" "}
            <Link to="/signin/new" search={{ next }} className={authLinkClass}>
              Create an account
            </Link>
          </>
        ) : (
          "No account yet? Ask an admin of your organization to invite you."
        ))
      }
    >
      <div className="grid gap-3">
        {failure && (
          <Alert variant={failure.neutral ? "info" : "destructive"} title={failure.title}>
            {failure.body}
          </Alert>
        )}
        {health.isError && (
          <Alert
            variant="warning"
            title="romulus is not answering"
            action={
              <Button size="sm" loading={health.isFetching} onClick={() => void health.refetch()}>
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
        {methods.isPending ? (
          <div className="grid gap-4" aria-busy>
            <Skeleton className="h-14 w-full" />
            <Skeleton className="h-14 w-full" />
            <Skeleton className="mt-2 h-10 w-full" />
          </div>
        ) : (
          password && (
            <PasswordSignInForm
              next={next}
              canReset={methods.data?.password_reset === true}
              defaultEmail={email}
            />
          )
        )}
        {password && sso && (
          <div className="my-1 flex items-center gap-3">
            <Separator className="flex-1" />
            <span className="text-sm text-faint-foreground">or</span>
            <Separator className="flex-1" />
          </div>
        )}
        {sso &&
          (ssoBlocked ? (
            <Button
              variant={password ? "secondary" : "primary"}
              size="lg"
              className="w-full"
              disabled
            >
              <LockIcon />
              Continue with SSO
            </Button>
          ) : (
            <Button
              variant={password ? "secondary" : "primary"}
              size="lg"
              className="w-full"
              asChild
            >
              <a href={loginUrl(next)}>
                <LockIcon />
                Continue with SSO
              </a>
            </Button>
          ))}
      </div>

      {sso && !password && (
        <p className="mt-4 text-sm text-muted-foreground">
          You sign in with your company account, then come straight back here.
        </p>
      )}

      {methods.data?.dev_signin && <DevSignin next={next} />}
    </AuthPage>
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
