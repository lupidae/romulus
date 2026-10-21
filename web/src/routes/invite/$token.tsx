import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link, redirect, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { LogoMark } from "@/components/brand/logo";
import {
  ClockIcon,
  InfoCircleIcon,
  LockIcon,
  RefreshDoubleIcon,
  WarningTriangleIcon,
} from "@/components/icons";
import { OrgTile } from "@/components/shell/org-switcher";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { authMethodsQuery, meQuery } from "@/features/auth/queries";
import { appPath, loginUrl, switchAccount } from "@/features/auth/session";
import { needsTerms, termsQuery } from "@/features/legal/queries";
import { useAcceptInvitation } from "@/features/orgs/mutations";
import { type InvitationPreview, invitationPreviewQuery } from "@/features/orgs/queries";
import { roleAbilities, withArticle } from "@/features/orgs/roles";
import { formatDate } from "@/lib/time";

// Where an invitation link lands. Works signed out (it says who is invited and
// asks to sign in), then tells the signed-in viewer exactly where they stand.
export const Route = createFileRoute("/invite/$token")({
  // Signed in without the Terms in force: them first, then back here
  // (joining is using the Service). Signed out: the page asks to sign in.
  beforeLoad: async ({ context: { queryClient }, location }) => {
    const me = await queryClient.ensureQueryData(meQuery).catch(() => null);
    if (!me) return;
    if (needsTerms(await queryClient.ensureQueryData(termsQuery))) {
      throw redirect({ to: "/signin/terms", search: { next: appPath(location.href) } });
    }
  },
  head: () => ({ meta: [{ title: "Invitation – romulus" }] }),
  component: AcceptInvitation,
});

function AcceptInvitation() {
  const { token } = Route.useParams();
  const preview = useQuery(invitationPreviewQuery(token));
  const notFound = preview.error instanceof ApiError && preview.error.status === 404;

  return (
    <div className="flex min-h-dvh flex-col bg-background">
      <header className="flex h-14 items-center px-5">
        <Link
          to="/"
          aria-label="romulus home"
          className="rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <LogoMark />
        </Link>
      </header>
      <main className="flex flex-1 items-start justify-center px-4 pt-[min(12vh,6rem)] pb-16">
        <Card className="w-full max-w-md p-6 sm:p-8">
          {preview.isPending ? (
            <PreviewSkeleton />
          ) : notFound ? (
            <DeadEnd
              icon={<WarningTriangleIcon />}
              title="This invitation link does not work"
              body="It may have been revoked, or replaced by a newer link. Ask an admin of the organization for a new one."
            />
          ) : preview.isError ? (
            <Alert
              variant="destructive"
              title="Could not open this invitation"
              action={
                <Button
                  size="sm"
                  loading={preview.isFetching}
                  onClick={() => void preview.refetch()}
                >
                  Retry
                </Button>
              }
            >
              {preview.error.message}
            </Alert>
          ) : (
            <Invitation token={token} invitation={preview.data} />
          )}
        </Card>
      </main>
    </div>
  );
}

function Invitation({ token, invitation }: { token: string; invitation: InvitationPreview }) {
  const { org, role, invited_by: invitedBy, status } = invitation;
  // Present while the link can be used, which every state below the early
  // returns is.
  const email = invitation.email ?? "the invited address";
  const here = appPath(`/invite/${token}`);
  const askFor = invitedBy ?? `an admin of ${org.name}`;

  // Links that can no longer do anything say so, and nothing else.
  if (status === "already_member") {
    return (
      <Outcome
        org={org.name}
        title={`You are already in ${org.name}`}
        body={
          invitation.member_role
            ? `You are ${withArticle(invitation.member_role)} there; this invitation changes nothing.`
            : "This invitation changes nothing."
        }
        action={
          <Button variant="primary" size="lg" className="w-full" asChild>
            <Link to="/$org" params={{ org: org.slug }}>
              Open {org.name}
            </Link>
          </Button>
        }
      />
    );
  }
  if (status === "expired" || status === "used") {
    return (
      <Outcome
        org={org.name}
        icon={status === "expired" ? <ClockIcon /> : <LockIcon />}
        title={
          status === "expired"
            ? `This invitation to ${org.name} expired`
            : `This invitation to ${org.name} was already used`
        }
        body={
          status === "expired"
            ? `Invitations last 7 days. Ask ${askFor} for a new link.`
            : `Each link works once. If you still need access, ask ${askFor} for a new one.`
        }
        action={
          <Button className="justify-self-start" asChild>
            <Link to="/">Go to romulus</Link>
          </Button>
        }
      />
    );
  }

  return (
    <div className="grid gap-6">
      <div className="grid justify-items-start gap-4">
        <OrgTile name={org.name} className="size-11 rounded-lg text-lg" />
        <div className="grid gap-1.5">
          <h1 className="text-2xl font-semibold text-balance text-foreground">
            Join {org.name} on romulus
          </h1>
          <p className="text-base text-muted-foreground">
            {invitedBy ?? "An admin"} invited{" "}
            <span className="font-medium text-foreground">{email}</span> to join as{" "}
            {withArticle(role)}.
          </p>
        </div>
        {/* What the role allows helps decide; once the account is wrong, it is noise. */}
        {(status === "sign_in_required" || status === "ready") && (
          <p className="flex items-start gap-2 rounded-md bg-muted px-3 py-2 text-sm text-muted-foreground">
            <InfoCircleIcon className="mt-0.5 size-4 shrink-0" />
            <span>{roleAbilities[role]}</span>
          </p>
        )}
      </div>
      {status === "sign_in_required" && (
        <SignInToAccept token={token} here={here} email={email} expiresAt={invitation.expires_at} />
      )}
      {status === "ready" && <Accept token={token} invitation={invitation} />}
      {status === "wrong_account" && <WrongAccount here={here} email={email} />}
      {status === "email_unverified" && (
        <div className="grid gap-3">
          <Alert variant="warning" title="Verify your email address first">
            romulus matches invitations to verified addresses. Verify {email} with your identity
            provider, then reload this page.
          </Alert>
          <Button size="lg" className="w-full" onClick={() => window.location.reload()}>
            <RefreshDoubleIcon />
            Reload
          </Button>
        </div>
      )}
    </div>
  );
}

/** Where a link ends: the organization, what happened, one way on. */
function Outcome({
  org,
  icon,
  title,
  body,
  action,
}: {
  org: string;
  icon?: React.ReactNode;
  title: string;
  body: string;
  action: React.ReactNode;
}) {
  return (
    <div className="grid gap-5">
      <div className="flex items-center gap-2">
        <OrgTile name={org} className="size-11 rounded-lg text-lg" />
        {icon && (
          <span className="-ml-4 flex size-7 items-center justify-center self-end rounded-full border-2 border-card bg-muted text-muted-foreground [&_svg]:size-3.5">
            {icon}
          </span>
        )}
      </div>
      <div className="grid gap-1.5">
        <h1 className="text-2xl font-semibold text-balance text-foreground">{title}</h1>
        <p className="text-base text-muted-foreground">{body}</p>
      </div>
      {action}
    </div>
  );
}

function SignInToAccept({
  token,
  here,
  email,
  expiresAt,
}: {
  token: string;
  here: string;
  email: string;
  expiresAt: string;
}) {
  const methods = useQuery(authMethodsQuery);
  if (methods.isPending) return <Skeleton className="h-10 w-full" />;
  if (methods.data?.password) {
    // Email + password: create the account with the invited address, or
    // sign in with it; single sign-on too when this server has both.
    return (
      <div className="grid gap-3">
        <Button variant="primary" size="lg" className="w-full" asChild>
          <Link to="/signin/new" search={{ invitation: token }}>
            Create your account
          </Link>
        </Button>
        <Button size="lg" className="w-full" asChild>
          <Link to="/signin" search={{ next: here, email }}>
            I already have an account
          </Link>
        </Button>
        {methods.data.sso && (
          <Button variant="ghost" className="w-full" asChild>
            <a href={loginUrl(here, { loginHint: email })}>
              <LockIcon />
              Continue with SSO
            </a>
          </Button>
        )}
        <p className="text-sm text-muted-foreground">
          Use {email}; you come straight back here. The invitation expires on{" "}
          {formatDate(expiresAt)}.
        </p>
      </div>
    );
  }
  return (
    <div className="grid gap-3">
      {/* Straight into single sign-on (in dev too): one click, and the
          invited address is pre-filled or offered for a new account. */}
      <Button variant="primary" size="lg" className="w-full" asChild>
        <a href={loginUrl(here, { loginHint: email })}>Sign in to accept</a>
      </Button>
      <p className="text-sm text-muted-foreground">
        Sign in, or create your account, with {email}; you come straight back here. The invitation
        expires on {formatDate(expiresAt)}.
      </p>
    </div>
  );
}

function Accept({ token, invitation }: { token: string; invitation: InvitationPreview }) {
  const accept = useAcceptInvitation(token);
  const navigate = useNavigate();
  const preview = useQuery(invitationPreviewQuery(token));
  const [error, setError] = React.useState<string>();

  return (
    <div className="grid gap-3">
      {error && (
        <Alert variant="destructive" title="Could not join">
          {error}
        </Alert>
      )}
      <Button
        variant="primary"
        size="lg"
        className="w-full"
        loading={accept.isPending}
        onClick={() =>
          accept.mutate(undefined, {
            onSuccess: ({ org }) => {
              toast.success(`Welcome to ${org.name}`, {
                description: `You joined as ${withArticle(org.role)}.`,
              });
              void navigate({ to: "/$org", params: { org: org.slug } });
            },
            onError: (err) => {
              setError(err instanceof ApiError ? err.message : "Something went wrong. Try again.");
              // The link may have changed state (used, expired): show what it is now.
              void preview.refetch();
            },
          })
        }
      >
        Join {invitation.org.name}
      </Button>
      <Button variant="ghost" className="w-full" asChild>
        <Link to="/">Not now</Link>
      </Button>
    </div>
  );
}

function WrongAccount({ here, email }: { here: string; email: string }) {
  const me = useQuery(meQuery);
  const methods = useQuery(authMethodsQuery);
  const [switching, setSwitching] = React.useState(false);
  // A password account, or a server without single sign-on: our own page.
  const sso = methods.data?.sso === true && me.data?.password_account !== true;

  return (
    <div className="grid gap-3">
      <Alert variant="warning" title={`This invitation is for ${email}`}>
        {me.data
          ? `You are signed in as ${me.data.email}.`
          : "You are signed in with another account."}
      </Alert>
      <Button
        variant="primary"
        size="lg"
        className="w-full"
        loading={switching}
        onClick={() => {
          setSwitching(true);
          switchAccount(here, email, { sso }).catch(() => {
            setSwitching(false);
            toast.error("Could not sign out. Try again.");
          });
        }}
      >
        Switch account
      </Button>
      <Button variant="ghost" className="w-full" asChild>
        <Link to="/">Stay signed in as {me.data?.email ?? "this account"}</Link>
      </Button>
    </div>
  );
}

/** An unknown link: nothing to show but the way out. */
function DeadEnd({ icon, title, body }: { icon: React.ReactNode; title: string; body: string }) {
  return (
    <div className="grid gap-5">
      <div className="flex size-11 items-center justify-center rounded-lg border border-border bg-card text-muted-foreground shadow-xs [&_svg]:size-5">
        {icon}
      </div>
      <div className="grid gap-1.5">
        <h1 className="text-2xl font-semibold text-balance text-foreground">{title}</h1>
        <p className="text-base text-muted-foreground">{body}</p>
      </div>
      <Button className="justify-self-start" asChild>
        <Link to="/">Go to romulus</Link>
      </Button>
    </div>
  );
}

function PreviewSkeleton() {
  return (
    <div className="grid gap-6" aria-busy>
      <div className="grid gap-4">
        <Skeleton className="size-11 rounded-lg" />
        <div className="grid gap-2">
          <Skeleton className="h-7 w-64 max-w-full" />
          <Skeleton className="h-4 w-full" />
        </div>
        <Skeleton className="h-9 w-full" />
      </div>
      <Skeleton className="h-10 w-full" />
    </div>
  );
}
