import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { KeyIcon, SparksIcon, TrashIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { CopyField } from "@/components/ui/copy-field";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { formatDate, formatDateTime, fromNow, timeAgo } from "@/lib/time";

import { scopeLabel } from "../labels";
import { useRevokeAccessToken } from "../mutations";
import { type AccessToken, accessTokensQuery, type CreatedAccessToken } from "../queries";
import { TokenForm } from "./token-form";

/** The person's access tokens: when each was last used, by what; revoke. */
export function AccessTokens({
  creating,
  onCreatingChange,
}: {
  creating: boolean;
  onCreatingChange: (open: boolean) => void;
}) {
  const tokens = useQuery(accessTokensQuery);
  const revoke = useRevokeAccessToken();
  const [revoking, setRevoking] = React.useState<AccessToken | null>(null);

  return (
    <Card>
      {tokens.isPending ? (
        <div aria-hidden className="divide-y divide-border">
          {[0, 1].map((i) => (
            <div key={i} className="flex items-center gap-3 px-4 py-3">
              <Skeleton className="size-4 rounded-sm" />
              <div className="grid flex-1 gap-1.5">
                <Skeleton className="h-4 w-40" />
                <Skeleton className="h-3 w-72 max-w-full" />
              </div>
            </div>
          ))}
        </div>
      ) : tokens.isError ? (
        <div className="p-4">
          <Alert
            variant="destructive"
            title="Could not load your tokens"
            action={
              <Button size="sm" loading={tokens.isFetching} onClick={() => void tokens.refetch()}>
                Retry
              </Button>
            }
          >
            {tokens.error.message}
          </Alert>
        </div>
      ) : tokens.data.length === 0 ? (
        <EmptyState
          className="py-10"
          icon={<KeyIcon />}
          title="No access tokens"
          description="Connect an AI agent to romulus: it gets a token of its own, which you can revoke here."
          action={
            <Button variant="primary" asChild>
              <Link to="/settings/agent">
                <SparksIcon />
                Connect your agent
              </Link>
            </Button>
          }
        />
      ) : (
        <ul aria-label="Access tokens" className="divide-y divide-border">
          {tokens.data.map((token) => (
            <li key={token.id} className="flex items-center gap-3 px-4 py-3">
              <KeyIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
              <div className="grid min-w-0 flex-1 gap-0.5">
                <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
                  <span className="truncate text-base font-medium text-foreground">
                    {token.name}
                  </span>
                  <Badge variant={token.scope === "write" ? "info" : "neutral"}>
                    {scopeLabel[token.scope]}
                  </Badge>
                  {token.expired && <Badge variant="warning">Expired</Badge>}
                </span>
                <span className="text-sm text-muted-foreground">
                  <LastUsed token={token} />
                  {" · "}
                  <Expiry token={token} />
                </span>
              </div>
              <Tooltip content="Revoke">
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={`Revoke ${token.name}`}
                  onClick={() => setRevoking(token)}
                >
                  <TrashIcon />
                </Button>
              </Tooltip>
            </li>
          ))}
        </ul>
      )}

      <CreateTokenDialog open={creating} onOpenChange={onCreatingChange} />
      <ConfirmDialog
        open={revoking !== null}
        onOpenChange={(open) => !open && setRevoking(null)}
        title={`Revoke ${revoking?.name ?? "this token"}?`}
        description="The agent using it loses access to romulus at once. What it already changed stays, with your name. This cannot be undone."
        confirmLabel="Revoke token"
        destructive
        onConfirm={() => {
          const token = revoking;
          if (!token) return;
          setRevoking(null);
          revoke.mutate(token.id, {
            onSuccess: () => toast.success(`${token.name} revoked`),
            onError: (error) =>
              toast.error(`Could not revoke ${token.name}`, {
                description: error instanceof ApiError ? error.message : "Check your connection.",
              }),
          });
        }}
      />
    </Card>
  );
}

function LastUsed({ token }: { token: AccessToken }) {
  if (!token.last_used_at) return <>Never used</>;
  return (
    <span title={formatDateTime(token.last_used_at)}>
      Last used {timeAgo(token.last_used_at)}
      {token.last_client && ` by ${token.last_client}`}
    </span>
  );
}

function Expiry({ token }: { token: AccessToken }) {
  if (!token.expires_at) return <>Never expires</>;
  if (token.expired) return <>Expired {formatDate(token.expires_at)}</>;
  return <span title={formatDateTime(token.expires_at)}>Expires {fromNow(token.expires_at)}</span>;
}

function CreateTokenDialog({
  open,
  onOpenChange,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  // The secret lives only while the dialog that showed it is open: every
  // way of closing forgets it, and every opening starts from the form.
  const [created, setCreated] = React.useState<CreatedAccessToken | null>(null);
  const [shownOpen, setShownOpen] = React.useState(open);
  if (open !== shownOpen) {
    setShownOpen(open);
    if (open) setCreated(null);
  }
  const close = () => {
    setCreated(null);
    onOpenChange(false);
  };
  return (
    <Dialog open={open} onOpenChange={(next) => (next ? onOpenChange(true) : close())}>
      <DialogContent className="max-w-lg">
        {created ? (
          <>
            <DialogHeader>
              <DialogTitle>Copy your token</DialogTitle>
              <DialogDescription>
                Paste it into your agent&apos;s settings. It acts as you, with your roles.
              </DialogDescription>
            </DialogHeader>
            <CopyField value={created.secret} label="Access token" autoCopy />
            <Alert variant="warning" title="It is shown only this once">
              romulus keeps only a hash of it. If you lose it, revoke it and create another.
            </Alert>
            <DialogFooter>
              <Button asChild>
                <Link to="/settings/agent">How to connect an agent</Link>
              </Button>
              <Button variant="primary" onClick={close}>
                Done
              </Button>
            </DialogFooter>
          </>
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>New access token</DialogTitle>
              <DialogDescription>
                For one AI agent, acting as you in every organization you belong to.
              </DialogDescription>
            </DialogHeader>
            {open && (
              <TokenForm onCreated={setCreated}>
                {(submitting) => (
                  <DialogFooter>
                    <Button onClick={close}>Cancel</Button>
                    <Button type="submit" variant="primary" loading={submitting}>
                      Create token
                    </Button>
                  </DialogFooter>
                )}
              </TokenForm>
            )}
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
