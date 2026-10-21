import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import * as React from "react";

import {
  CheckCircleIcon,
  NavArrowDownIcon,
  NavArrowRightIcon,
  OpenNewWindowIcon,
} from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { CodeBlock } from "@/components/ui/code-block";
import { CopyField } from "@/components/ui/copy-field";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { timeAgo } from "@/lib/time";
import { cn } from "@/lib/utils";

import { type ConnectionCheck, checkConnection } from "../connection";
import { DOCS_URL, scopeLabel } from "../labels";
import {
  accessTokensQuery,
  type ConnectedApp,
  connectedAppsQuery,
  type CreatedAccessToken,
} from "../queries";
import {
  type AgentClient,
  endpoint,
  snippet,
  TOKEN_PLACEHOLDER,
  type TokenClient,
  tokenSnippet,
} from "../snippets";
import { UNNAMED_APP } from "./consent";
import { TokenForm } from "./token-form";

const clients: { value: AgentClient; label: string }[] = [
  { value: "claude-code", label: "Claude Code" },
  { value: "claude", label: "Claude" },
  { value: "cursor", label: "Cursor" },
  { value: "other", label: "Other" },
];

const tokenClients: { value: TokenClient; label: string }[] = [
  { value: "claude-code", label: "Claude Code" },
  { value: "claude-desktop", label: "Claude Desktop" },
  { value: "cursor", label: "Cursor" },
  { value: "other", label: "Other" },
];

/**
 * Connecting an AI agent: add romulus to it, then approve it when it opens
 * this browser (OAuth), seen arriving in the connected apps. Agents that
 * cannot open a browser (CI, scripts) take an access token instead: the
 * Advanced section, its own three steps.
 */
export function ConnectAgent() {
  const [client, setClient] = React.useState<AgentClient>("claude-code");
  const [advanced, setAdvanced] = React.useState(false);
  const url = endpoint(window.location.origin);
  const advancedId = React.useId();

  return (
    <div className="grid gap-4">
      <ol className="grid grid-cols-1 gap-4" aria-label="Steps">
        <Step number={1} title="Add romulus to your agent">
          <Tabs
            className="min-w-0"
            value={client}
            onValueChange={(v) => setClient(v as AgentClient)}
          >
            <TabsList aria-label="Agent" className="max-w-full overflow-x-auto">
              {clients.map((c) => (
                <TabsTrigger key={c.value} value={c.value}>
                  {c.label}
                </TabsTrigger>
              ))}
            </TabsList>
            {clients.map((c) => {
              const shown = snippet(c.value, url);
              return (
                <TabsContent key={c.value} value={c.value} className="grid gap-2">
                  <p className="text-sm text-muted-foreground">{shown.where}</p>
                  <CodeBlock code={shown.code} label={shown.label} />
                </TabsContent>
              );
            })}
          </Tabs>
        </Step>

        <Step
          number={2}
          title="Approve it in your browser"
          description="Your agent opens this browser. Check its name and where it sends you back, then Allow."
        >
          <Arrival />
        </Step>
      </ol>

      <Card className="p-0">
        <Button
          variant="ghost"
          className="h-auto w-full justify-start gap-3 rounded-lg px-4 py-3 text-left whitespace-normal sm:h-auto sm:px-5"
          aria-expanded={advanced}
          aria-controls={advancedId}
          onClick={() => setAdvanced((open) => !open)}
        >
          {advanced ? <NavArrowDownIcon aria-hidden /> : <NavArrowRightIcon aria-hidden />}
          <span className="grid gap-0.5">
            <span className="text-base font-semibold text-foreground">
              Advanced: connect with an access token
            </span>
            <span className="text-sm font-normal text-muted-foreground">
              For CI, scripts and agents that cannot open a browser.
            </span>
          </span>
        </Button>
        {advanced && (
          <div id={advancedId} className="border-t border-border p-4 sm:p-5">
            <TokenSetup url={url} />
          </div>
        )}
      </Card>

      <p className="text-sm">
        <a
          href={DOCS_URL}
          target="_blank"
          rel="noreferrer"
          className="inline-flex items-center gap-1 font-medium text-primary underline-offset-4 hover:underline"
        >
          Every tool, and how access works
          <OpenNewWindowIcon aria-hidden className="size-3.5" />
        </a>
      </p>
    </div>
  );
}

/**
 * The newest app the person connected since this page opened: what the
 * agent's sign-in made, seen as it lands.
 */
function Arrival() {
  const [since] = React.useState(() => Date.now());
  const apps = useQuery({ ...connectedAppsQuery, refetchInterval: 5000 });
  const arrived: ConnectedApp | undefined = apps.data?.find(
    (app) => Date.parse(app.connected_at) >= since - 5000,
  );
  return (
    <div className="grid gap-2">
      <p role="status" aria-live="polite" className="text-sm text-muted-foreground">
        {arrived ? (
          <>
            <CheckCircleIcon aria-hidden className="mr-1 inline size-3.5 text-success" />
            {arrived.name ?? UNNAMED_APP} connected {timeAgo(arrived.connected_at)} (
            {scopeLabel[arrived.scope].toLowerCase()}). Ask it: &ldquo;What databases do we keep in
            romulus?&rdquo;
          </>
        ) : (
          <>Waiting for your agent to connect.</>
        )}
      </p>
      <p className="text-sm text-muted-foreground">
        Approved apps are listed in{" "}
        <Link
          to="/settings/apps"
          className="font-medium text-foreground underline decoration-border-strong underline-offset-4 hover:decoration-foreground"
        >
          Connected apps
        </Link>
        .
      </p>
    </div>
  );
}

/**
 * Connecting with a token, in three steps: a token of its own, the agent's
 * configuration with the endpoint and that token filled in, and a check
 * that it works (from this browser, then the agent's own first request,
 * seen as the token's last use).
 */
function TokenSetup({ url }: { url: string }) {
  const [created, setCreated] = React.useState<CreatedAccessToken | null>(null);
  const [client, setClient] = React.useState<TokenClient>("claude-code");
  const token = created?.secret ?? TOKEN_PLACEHOLDER;

  return (
    <ol className="grid grid-cols-1 gap-6" aria-label="Steps with a token">
      <SubStep
        number={1}
        title="Create a token for your agent"
        done={created !== null}
        description="It acts as you, with your role in each organization."
      >
        {created ? (
          <div className="grid gap-3">
            <p className="text-sm text-muted-foreground">
              <span className="font-medium text-foreground">{created.token.name}</span>,{" "}
              {scopeLabel[created.token.scope].toLowerCase()}. Shown only here: copy it now. The
              configurations below hold it; Claude Code&rsquo;s command asks for it.
            </p>
            <CopyField value={created.secret} label="Access token" />
          </div>
        ) : (
          <TokenForm defaultName="Claude Code" onCreated={setCreated}>
            {(submitting) => (
              <div>
                <Button type="submit" variant="primary" loading={submitting}>
                  Create token
                </Button>
              </div>
            )}
          </TokenForm>
        )}
      </SubStep>

      <SubStep number={2} title="Add romulus to your agent, with the token">
        <Tabs className="min-w-0" value={client} onValueChange={(v) => setClient(v as TokenClient)}>
          <TabsList aria-label="Agent for the token" className="max-w-full overflow-x-auto">
            {tokenClients.map((c) => (
              <TabsTrigger key={c.value} value={c.value}>
                {c.label}
              </TabsTrigger>
            ))}
          </TabsList>
          {tokenClients.map((c) => {
            const shown = tokenSnippet(c.value, url, token);
            return (
              <TabsContent key={c.value} value={c.value} className="grid gap-2">
                <p className="text-sm text-muted-foreground">{shown.where}</p>
                <CodeBlock code={shown.code} label={shown.label} />
              </TabsContent>
            );
          })}
        </Tabs>
        {!created && tokenSnippet(client, url, token).code.includes(TOKEN_PLACEHOLDER) && (
          <p className="text-sm text-muted-foreground">
            The token goes in place of {TOKEN_PLACEHOLDER}.
          </p>
        )}
      </SubStep>

      <SubStep number={3} title="Check that it works">
        <Check url={url} created={created} />
      </SubStep>
    </ol>
  );
}

function Step({
  number,
  title,
  description,
  children,
}: {
  number: number;
  title: string;
  description?: string;
  children: React.ReactNode;
}) {
  const id = React.useId();
  return (
    <li aria-labelledby={id}>
      <Card className="grid grid-cols-1 gap-4 p-4 sm:p-5">
        <StepHeading id={id} number={number} title={title} description={description} />
        <div className="grid min-w-0 grid-cols-1 gap-3 sm:pl-9">{children}</div>
      </Card>
    </li>
  );
}

/** A step inside the Advanced section: the same heading, no card of its own. */
function SubStep({
  number,
  title,
  description,
  done = false,
  children,
}: {
  number: number;
  title: string;
  description?: string;
  done?: boolean;
  children: React.ReactNode;
}) {
  const id = React.useId();
  return (
    <li aria-labelledby={id} className="grid grid-cols-1 gap-4">
      <StepHeading id={id} number={number} title={title} description={description} done={done} />
      <div className="grid min-w-0 grid-cols-1 gap-3 sm:pl-9">{children}</div>
    </li>
  );
}

function StepHeading({
  id,
  number,
  title,
  description,
  done = false,
}: {
  id: string;
  number: number;
  title: string;
  description?: string;
  done?: boolean;
}) {
  return (
    <div className="flex items-start gap-3">
      <span
        aria-hidden
        className={cn(
          "flex size-6 shrink-0 items-center justify-center rounded-full text-sm font-semibold tabular-nums",
          done ? "bg-success/10 text-success" : "bg-muted text-muted-foreground",
        )}
      >
        {done ? <CheckCircleIcon className="size-4" /> : number}
      </span>
      <div className="grid min-w-0 gap-0.5">
        <h2 id={id} className="text-base font-semibold text-foreground">
          {title}
          {done && <span className="sr-only"> (done)</span>}
        </h2>
        {description && <p className="text-sm text-muted-foreground">{description}</p>}
      </div>
    </div>
  );
}

/**
 * The check from this browser, and the agent's own first request: the
 * token's last use by a client other than this page.
 */
function Check({ url, created }: { url: string; created: CreatedAccessToken | null }) {
  const [result, setResult] = React.useState<ConnectionCheck | null>(null);
  const [checking, setChecking] = React.useState(false);
  const tokens = useQuery({
    ...accessTokensQuery,
    enabled: created !== null,
    refetchInterval: 5000,
  });
  const used = tokens.data?.find((t) => t.id === created?.token.id);
  const agent =
    used?.last_used_at && used.last_client && !used.last_client.startsWith("romulus-web")
      ? used
      : null;

  if (!created) {
    return <p className="text-sm text-muted-foreground">Available once the token exists.</p>;
  }
  const run = async () => {
    setChecking(true);
    setResult(await checkConnection(url, created.secret));
    setChecking(false);
  };
  return (
    <div className="grid gap-3">
      <div>
        <Button variant="primary" loading={checking} onClick={() => void run()}>
          Test connection
        </Button>
      </div>
      {result &&
        (result.ok ? (
          <Alert variant="success" title={`Connected as ${result.name}`}>
            romulus answers at {url} with {result.tools} tools for this token.
          </Alert>
        ) : (
          <Alert variant="destructive" title="The connection failed">
            {result.message}
          </Alert>
        ))}
      <p role="status" aria-live="polite" className="text-sm text-muted-foreground">
        {agent?.last_used_at ? (
          <>
            <CheckCircleIcon aria-hidden className="mr-1 inline size-3.5 text-success" />
            Your agent connected {timeAgo(agent.last_used_at)} ({agent.last_client}). Ask it to plan
            a safe migration next.
          </>
        ) : (
          <>
            Waiting for your agent. Restart it, then ask: &ldquo;What databases do we keep in
            romulus?&rdquo;
          </>
        )}
      </p>
    </div>
  );
}
