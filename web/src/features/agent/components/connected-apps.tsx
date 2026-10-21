import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { AppWindowIcon, ComputerIcon, SparksIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { formatDate, formatDateTime, timeAgo } from "@/lib/time";

import { scopeLabel } from "../labels";
import { useDisconnectApp } from "../mutations";
import { type ConnectedApp, connectedAppsQuery } from "../queries";
import { UNNAMED_APP } from "./consent";

/** The apps the person connected by signing in: where, what access, when used; Disconnect. */
export function ConnectedApps() {
  const apps = useQuery(connectedAppsQuery);
  const disconnect = useDisconnectApp();
  const [leaving, setLeaving] = React.useState<ConnectedApp | null>(null);
  const name = (app: ConnectedApp | null) => app?.name ?? UNNAMED_APP;

  return (
    <Card>
      {apps.isPending ? (
        <div aria-hidden className="divide-y divide-border">
          {[0, 1].map((i) => (
            <div key={i} className="flex items-center gap-3 px-4 py-3">
              <Skeleton className="size-4 rounded-sm" />
              <div className="grid flex-1 gap-1.5">
                <Skeleton className="h-4 w-40" />
                <Skeleton className="h-3 w-72 max-w-full" />
              </div>
              <Skeleton className="h-8 w-24" />
            </div>
          ))}
        </div>
      ) : apps.isError ? (
        <div className="p-4">
          <Alert
            variant="destructive"
            title="Could not load your connected apps"
            action={
              <Button size="sm" loading={apps.isFetching} onClick={() => void apps.refetch()}>
                Retry
              </Button>
            }
          >
            {apps.error.message}
          </Alert>
        </div>
      ) : apps.data.length === 0 ? (
        <EmptyState
          className="py-10"
          icon={<AppWindowIcon />}
          title="No connected apps"
          description="When you approve an AI agent in your browser, it shows here, and you can disconnect it."
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
        <ul aria-label="Connected apps" className="divide-y divide-border">
          {apps.data.map((app) => (
            <li key={app.client_id} className="flex items-start gap-3 px-4 py-3">
              <span aria-hidden className="mt-1 text-muted-foreground [&_svg]:size-4">
                {app.loopback ? <ComputerIcon /> : <AppWindowIcon />}
              </span>
              <div className="flex min-w-0 flex-1 flex-wrap items-center gap-x-3 gap-y-2">
                <div className="grid min-w-0 flex-1 basis-56 gap-0.5">
                  <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
                    <span className="truncate text-base font-medium text-foreground">
                      {name(app)}
                    </span>
                    <Badge variant={app.scope === "write" ? "info" : "neutral"}>
                      {scopeLabel[app.scope]}
                    </Badge>
                  </span>
                  <span className="text-sm text-muted-foreground">
                    {app.loopback ? `On your computer (${app.redirect_host})` : app.redirect_host}
                    {" · "}
                    <LastUsed app={app} />
                    {" · "}
                    <span title={formatDateTime(app.connected_at)}>
                      Connected {formatDate(app.connected_at)}
                    </span>
                  </span>
                </div>
                <Button
                  size="sm"
                  aria-label={`Disconnect ${name(app)}`}
                  onClick={() => setLeaving(app)}
                >
                  Disconnect
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}

      <ConfirmDialog
        open={leaving !== null}
        onOpenChange={(open) => !open && setLeaving(null)}
        title={`Disconnect ${name(leaving)}?`}
        description="It loses access at once; connect it again from your agent to use it. What it changed stays."
        confirmLabel="Disconnect"
        destructive
        onConfirm={() => {
          const app = leaving;
          if (!app) return;
          setLeaving(null);
          disconnect.mutate(app.client_id, {
            onSuccess: () => toast.success(`${name(app)} disconnected`),
            onError: (error) =>
              toast.error(`Could not disconnect ${name(app)}`, {
                description: error instanceof ApiError ? error.message : "Check your connection.",
              }),
          });
        }}
      />
    </Card>
  );
}

function LastUsed({ app }: { app: ConnectedApp }) {
  if (!app.last_used_at) return <>Not used yet</>;
  return (
    <span title={formatDateTime(app.last_used_at)}>
      Last used {timeAgo(app.last_used_at)}
      {app.last_client && ` by ${app.last_client}`}
    </span>
  );
}
