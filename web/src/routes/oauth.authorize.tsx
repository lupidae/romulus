import { createFileRoute, Link } from "@tanstack/react-router";

import { LogoMark } from "@/components/brand/logo";
import { Card } from "@/components/ui/card";
import { Consent, ConsentDeadEnd, ConsentRefused } from "@/features/agent/components/consent";
import { requireUser } from "@/features/auth/session";

interface AuthorizeSearch {
  /** The request `/api/oauth/authorize` kept for this browser. */
  request?: string;
  /** Why the API could not even ask (an unknown app, a redirect it never registered). */
  error?: string;
  /**
   * A request the API refused (PKCE, the scope…), with what names its way
   * back: checked again by the API before the page offers it.
   */
  refused?: string;
  client_id?: string;
  redirect_uri?: string;
  state?: string;
}

const text = (value: unknown) => (typeof value === "string" ? value : undefined);

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

// Where an agent's "connect to romulus" lands (OAuth, crates/romulus-api/src/oauth):
// the API checked the request and sends the browser here to ask the person.
// Signing in comes first, and comes back.
export const Route = createFileRoute("/oauth/authorize")({
  validateSearch: (search: Record<string, unknown>): AuthorizeSearch => ({
    request:
      typeof search.request === "string" && UUID.test(search.request) ? search.request : undefined,
    error: text(search.error),
    refused: text(search.refused),
    client_id: text(search.client_id),
    redirect_uri: text(search.redirect_uri),
    state: text(search.state),
  }),
  beforeLoad: async ({ context: { queryClient }, location, search }) => {
    // A dead end explains itself to anyone; a request needs its person.
    if (search.request && !search.error) await requireUser(queryClient, location.href);
  },
  head: () => ({ meta: [{ title: "Connect an app – romulus" }] }),
  component: AuthorizePage,
});

function AuthorizePage() {
  const search = Route.useSearch();
  const { request, error, refused } = search;
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
      <main className="flex flex-1 items-start justify-center px-4 pt-[min(10vh,5rem)] pb-16">
        <Card className="w-full max-w-md p-6 sm:p-8">
          {request && !error ? (
            <Consent key={request} id={request} />
          ) : refused && search.client_id && search.redirect_uri && !error ? (
            <ConsentRefused
              reason={refused}
              clientId={search.client_id}
              redirectUri={search.redirect_uri}
              state={search.state}
            />
          ) : (
            <ConsentDeadEnd code={error ?? "invalid_request"} />
          )}
        </Card>
      </main>
    </div>
  );
}
