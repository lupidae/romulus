import { useQuery } from "@tanstack/react-query";
import * as React from "react";

import { ApiError } from "@/api/client";
import { KeyIcon, PlusIcon, TrashIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { CodeBlock } from "@/components/ui/code-block";
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
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { formatDate, formatDateTime, timeAgo } from "@/lib/time";

import { useCreateToken, useRevokeToken } from "../mutations";
import {
  type ApiToken,
  type CreatedToken,
  type Database,
  tokensQuery,
  type TokenScope,
} from "../queries";
import { pushCommand } from "../push-command";

const scopeLabel: Record<TokenScope, string> = { push: "Push", full: "Push and read" };
const scopeDescription: Record<TokenScope, string> = {
  push: "Creates versions. Enough for CI and remus.",
  full: "Creates versions and reads them back.",
};

/** The database's API tokens: create one (its secret shown once), see them, revoke. */
export function ApiTokens({ org, database }: { org: string; database: Database }) {
  const db = database.slug;
  const tokens = useQuery(tokensQuery(org, db));
  const revoke = useRevokeToken(org, db);
  const [creating, setCreating] = React.useState(false);
  const [revoking, setRevoking] = React.useState<ApiToken | null>(null);

  return (
    <Card id="tokens" className="scroll-mt-4">
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle>API tokens</CardTitle>
          <p className="text-sm text-muted-foreground">
            How remus and CI push versions. A token works for this database only.
          </p>
        </div>
        {tokens.data && tokens.data.length > 0 && (
          <Button size="sm" onClick={() => setCreating(true)}>
            <PlusIcon />
            New token
          </Button>
        )}
      </CardHeader>
      {tokens.isPending ? (
        <div aria-hidden className="divide-y divide-border">
          {[0, 1].map((i) => (
            <div key={i} className="flex items-center gap-3 px-4 py-3">
              <Skeleton className="size-5 rounded-full" />
              <div className="grid flex-1 gap-1.5">
                <Skeleton className="h-4 w-36" />
                <Skeleton className="h-3 w-56" />
              </div>
            </div>
          ))}
        </div>
      ) : tokens.isError ? (
        <div className="p-4">
          <Alert
            variant="destructive"
            title="Could not load the tokens"
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
          title="No tokens yet"
          description="Create one, then give it to CI as ROMULUS_TOKEN."
          action={
            <Button variant="primary" onClick={() => setCreating(true)}>
              <PlusIcon />
              New token
            </Button>
          }
        />
      ) : (
        <ul aria-label="API tokens" className="divide-y divide-border">
          {tokens.data.map((token) => (
            <li key={token.id} className="flex items-center gap-3 px-4 py-3">
              <KeyIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
              <div className="grid min-w-0 flex-1 gap-0.5">
                <span className="flex min-w-0 items-center gap-2">
                  <span className="truncate text-base font-medium text-foreground">
                    {token.name}
                  </span>
                  <Badge variant={token.scope === "full" ? "info" : "neutral"}>
                    {scopeLabel[token.scope]}
                  </Badge>
                </span>
                <span className="truncate text-sm text-muted-foreground">
                  {token.last_used_at ? (
                    <span title={formatDateTime(token.last_used_at)}>
                      Last used {timeAgo(token.last_used_at)}
                    </span>
                  ) : (
                    "Never used"
                  )}
                  {" · "}
                  Created {formatDate(token.created_at)}
                  {token.created_by && ` by ${token.created_by}`}
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

      <CreateTokenDialog org={org} database={database} open={creating} onOpenChange={setCreating} />
      <ConfirmDialog
        open={revoking !== null}
        onOpenChange={(open) => !open && setRevoking(null)}
        title={`Revoke ${revoking?.name ?? "this token"}?`}
        description={`Whatever uses it can no longer push to ${database.name}. Versions it pushed keep its name. This cannot be undone.`}
        confirmLabel="Revoke token"
        destructive
        onConfirm={() => {
          const token = revoking;
          if (!token) return;
          // Optimistic: gone from the list at once, back if the API refuses.
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

function CreateTokenDialog({
  org,
  database,
  open,
  onOpenChange,
}: {
  org: string;
  database: Database;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  // The secret lives only while the dialog that showed it is open: every
  // way of closing forgets it, and every opening starts from the form
  // (adjusted during render, so a reopened dialog never paints the old one).
  const [created, setCreated] = React.useState<CreatedToken | null>(null);
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
                {created.token.name} can now push to {database.name}. Store it as a secret named
                ROMULUS_TOKEN where CI runs.
              </DialogDescription>
            </DialogHeader>
            <CopyField value={created.secret} label="API token" autoCopy />
            <Alert variant="warning" title="It is shown only this once">
              romulus keeps only a hash of it. If you lose it, revoke it and create another.
            </Alert>
            <div className="grid gap-1.5">
              <p className="text-sm font-medium text-foreground">Then push from CI</p>
              <CodeBlock code={pushCommand(database.id)} label="Push command" />
            </div>
            <DialogFooter>
              <Button variant="primary" onClick={close}>
                Done
              </Button>
            </DialogFooter>
          </>
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>New API token</DialogTitle>
              <DialogDescription>For one database: {database.name}.</DialogDescription>
            </DialogHeader>
            {open && (
              <CreateTokenForm
                org={org}
                db={database.slug}
                onCreated={setCreated}
                onCancel={close}
              />
            )}
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}

function CreateTokenForm({
  org,
  db,
  onCreated,
  onCancel,
}: {
  org: string;
  db: string;
  onCreated: (created: CreatedToken) => void;
  onCancel: () => void;
}) {
  const create = useCreateToken(org, db);
  const [name, setName] = React.useState("");
  const [scope, setScope] = React.useState<TokenScope>("push");
  const [error, setError] = React.useState<string>();

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    if (!name.trim()) return setError("Name it after where it is used, e.g. GitHub Actions.");
    create.mutate(
      { name: name.trim(), scope },
      {
        onSuccess: onCreated,
        onError: (err) =>
          setError(
            err instanceof ApiError
              ? (err.fields.name ?? err.message)
              : "Could not create the token. Try again.",
          ),
      },
    );
  };

  return (
    <form noValidate onSubmit={submit} className="grid gap-4">
      <Field label="Name" error={error} description="Where it is used.">
        <Input
          placeholder="GitHub Actions"
          maxLength={100}
          autoComplete="off"
          value={name}
          onChange={(e) => {
            setName(e.target.value);
            setError(undefined);
          }}
        />
      </Field>
      <Field label="Access">
        <Select value={scope} onValueChange={(v) => setScope(v as TokenScope)}>
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {(["push", "full"] as const).map((s) => (
              <SelectItem key={s} value={s} description={scopeDescription[s]}>
                {scopeLabel[s]}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Field>
      <DialogFooter>
        <Button onClick={onCancel}>Cancel</Button>
        <Button type="submit" variant="primary" loading={create.isPending}>
          Create token
        </Button>
      </DialogFooter>
    </form>
  );
}
