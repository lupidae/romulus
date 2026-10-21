import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { AppWindowIcon, ComputerIcon, WarningTriangleIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { RadioCard, RadioGroup } from "@/components/ui/radio-group";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { toast } from "@/components/ui/toaster";
import { authMethodsQuery, meQuery } from "@/features/auth/queries";
import { appPath, switchAccount } from "@/features/auth/session";

import { scopeDescription, scopeLabel } from "../labels";
import { useDecideAuthorization } from "../mutations";
import {
  type AccessTokenScope,
  type AuthorizationRequest,
  authorizationRequestQuery,
} from "../queries";

/** What an app that gave no name is called (the API's `UNNAMED` too). */
export const UNNAMED_APP = "An unnamed app";

/**
 * An app asking to work in romulus as the signed-in person (OAuth): who
 * asks, where the answer goes (the one part romulus can vouch for), what it
 * may do, and Allow or Deny. Deciding sends the browser back to the app.
 */
export function Consent({ id }: { id: string }) {
  const request = useQuery(authorizationRequestQuery(id));
  if (request.isPending) return <ConsentSkeleton />;
  if (request.isError) {
    if (request.error instanceof ApiError && request.error.status === 404) {
      return <ConsentDeadEnd code="request_gone" />;
    }
    return (
      <Alert
        variant="destructive"
        title="Could not open this request"
        action={
          <Button size="sm" loading={request.isFetching} onClick={() => void request.refetch()}>
            Retry
          </Button>
        }
      >
        {request.error.message}
      </Alert>
    );
  }
  return <Decide request={request.data} />;
}

function Decide({ request }: { request: AuthorizationRequest }) {
  const me = useQuery(meQuery);
  const methods = useQuery(authMethodsQuery);
  const decide = useDecideAuthorization(request.id);
  const [scope, setScope] = React.useState<AccessTokenScope>(request.scope);
  const [left, setLeft] = React.useState<"allowed" | "denied" | null>(null);
  const [switching, setSwitching] = React.useState(false);
  const { client } = request;
  const name = client.name ?? UNNAMED_APP;

  const answer = (allow: boolean) =>
    decide.mutate(
      { allow, scope: allow ? scope : undefined },
      {
        onSuccess: ({ redirect_url }) => {
          setLeft(allow ? "allowed" : "denied");
          window.location.assign(redirect_url);
        },
        onError: (error) =>
          toast.error("Could not answer the app", {
            description:
              error instanceof ApiError ? error.message : "Check your connection and try again.",
          }),
      },
    );

  if (left) {
    return (
      <div className="grid gap-5" role="status">
        <AppTile loopback={client.loopback} />
        <div className="grid gap-1.5">
          <h1 className="text-2xl font-semibold text-balance text-foreground">
            {left === "allowed" ? `${name} is connected` : `${name} was not connected`}
          </h1>
          <p className="flex items-center gap-2 text-base text-muted-foreground">
            <Spinner className="size-4" />
            Taking you back to {client.redirect_host}…
          </p>
        </div>
        <p className="text-sm text-muted-foreground">
          {left === "allowed"
            ? "Your agent finishes connecting there. Once it says so, you can close this tab."
            : "Nothing was shared with it. You can close this tab."}
        </p>
      </div>
    );
  }

  // A password account, or a server without single sign-on: our own page.
  const sso = methods.data?.sso === true && me.data?.password_account !== true;
  const here = appPath(`/oauth/authorize?request=${request.id}`);
  return (
    <div className="grid gap-6">
      <div className="grid justify-items-start gap-4">
        <AppTile loopback={client.loopback} />
        <div className="grid gap-1.5">
          <h1 className="text-2xl font-semibold text-balance text-foreground">
            Allow {name} to use romulus?
          </h1>
          <p className="text-base text-pretty text-muted-foreground">
            It will act as you, with your role in each organization.
          </p>
        </div>
      </div>

      <dl className="grid gap-3 rounded-lg border border-border px-4 py-3 text-sm">
        <div className="grid gap-0.5">
          <dt className="text-muted-foreground">Sends you back to</dt>
          <dd className="font-medium break-all text-foreground">
            {client.redirect_host}
            {client.loopback && (
              <span className="font-normal break-normal text-muted-foreground">
                , a program on this computer
              </span>
            )}
          </dd>
        </div>
        <div className="grid gap-0.5">
          <dt className="text-muted-foreground">Signed in as</dt>
          <dd className="flex flex-wrap items-baseline gap-x-2 font-medium text-foreground">
            <span className="break-all">{me.data?.email ?? "…"}</span>
            <Button
              variant="link"
              size="sm"
              className="text-sm"
              loading={switching}
              onClick={() => {
                setSwitching(true);
                switchAccount(here, undefined, { sso }).catch(() => {
                  setSwitching(false);
                  toast.error("Could not sign out. Try again.");
                });
              }}
            >
              Use another account
            </Button>
          </dd>
        </div>
      </dl>

      {request.scope === "write" ? (
        <fieldset className="grid gap-2">
          <legend className="mb-2 text-sm font-medium text-foreground">Access</legend>
          <RadioGroup value={scope} onValueChange={(value) => setScope(value as AccessTokenScope)}>
            <RadioCard
              value="write"
              label={scopeLabel.write}
              description={scopeDescription.write}
            />
            <RadioCard value="read" label={scopeLabel.read} description={scopeDescription.read} />
          </RadioGroup>
        </fieldset>
      ) : (
        <div className="grid gap-0.5 rounded-lg border border-border px-4 py-3">
          <p className="text-sm font-medium text-foreground">{scopeLabel.read}</p>
          <p className="text-sm text-muted-foreground">{scopeDescription.read}</p>
        </div>
      )}

      {client.loopback && (
        <Alert variant="warning" title="Only if you just started connecting an agent">
          Any program on this computer can ask this way.
        </Alert>
      )}

      <div className="grid gap-2 sm:grid-cols-2">
        <Button
          variant="primary"
          size="lg"
          className="sm:order-2"
          loading={decide.isPending && decide.variables?.allow === true}
          disabled={decide.isPending}
          onClick={() => answer(true)}
        >
          Allow
        </Button>
        <Button
          size="lg"
          className="sm:order-1"
          loading={decide.isPending && decide.variables?.allow === false}
          disabled={decide.isPending}
          onClick={() => answer(false)}
        >
          Deny
        </Button>
      </div>
      <p className="text-sm text-muted-foreground">
        You can disconnect it later, in your account&rsquo;s Connected apps.
      </p>
    </div>
  );
}

function AppTile({ loopback }: { loopback: boolean }) {
  return (
    <span
      aria-hidden
      className="flex size-11 items-center justify-center rounded-lg border border-border bg-muted text-muted-foreground [&_svg]:size-5"
    >
      {loopback ? <ComputerIcon /> : <AppWindowIcon />}
    </span>
  );
}

interface DeadEndCopy {
  title: string;
  body: string;
}

const invalidRequest: DeadEndCopy = {
  title: "This link to connect an app is not valid",
  body: "Part of it is missing or repeated. Go back to your agent and connect again.",
};

/** What each way a request can fail before reaching the person means. */
const deadEnds: Record<string, DeadEndCopy> = {
  request_gone: {
    title: "This request is over",
    body: "It expired (requests last 20 minutes), was already answered, or was started in another browser. Go back to your agent and connect again.",
  },
  unknown_client: {
    title: "romulus does not know this app",
    body: "It may have been registered long ago and expired. Go back to your agent and connect again: it registers itself anew.",
  },
  invalid_redirect_uri: {
    title: "This app asked to go somewhere it did not register",
    body: "romulus did not send you there, and shared nothing. Go back to your agent and connect again.",
  },
  invalid_request: invalidRequest,
  unavailable: {
    title: "romulus could not open this request",
    body: "Something went wrong on romulus's side. Go back to your agent and try again in a moment.",
  },
};

/** A request that cannot be answered: what happened, and the way on. */
export function ConsentDeadEnd({ code }: { code: string }) {
  const copy = deadEnds[code] ?? invalidRequest;
  return (
    <div className="grid gap-5">
      <div className="flex size-11 items-center justify-center rounded-lg border border-border bg-card text-muted-foreground shadow-xs [&_svg]:size-5">
        <WarningTriangleIcon aria-hidden />
      </div>
      <div className="grid gap-1.5">
        <h1 className="text-2xl font-semibold text-balance text-foreground">{copy.title}</h1>
        <p className="text-base text-muted-foreground">{copy.body}</p>
      </div>
      <Button className="justify-self-start" asChild>
        <Link to="/">Go to romulus</Link>
      </Button>
    </div>
  );
}

function ConsentSkeleton() {
  return (
    <div className="grid gap-6" aria-busy>
      <div className="grid gap-4">
        <Skeleton className="size-11 rounded-lg" />
        <div className="grid gap-2">
          <Skeleton className="h-7 w-72 max-w-full" />
          <Skeleton className="h-4 w-full" />
        </div>
      </div>
      <Skeleton className="h-36 w-full rounded-lg" />
      <Skeleton className="h-28 w-full rounded-lg" />
      <div className="grid gap-2 sm:grid-cols-2">
        <Skeleton className="h-10 w-full" />
        <Skeleton className="h-10 w-full" />
      </div>
    </div>
  );
}
