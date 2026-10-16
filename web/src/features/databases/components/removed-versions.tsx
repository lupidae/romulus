import { useQuery } from "@tanstack/react-query";

import { ApiError } from "@/api/client";
import { EyeClosedIcon, UndoIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { formatDate, formatDateTime, timeAgo } from "@/lib/time";

import { useRestoreVersion } from "../mutations";
import { type Database, type RemovedVersion, removedVersionsQuery } from "../queries";
import { OriginBadge } from "./version-meta";

/**
 * The database's removed versions: who removed each and why, until when it
 * can come back, and Restore (admins).
 */
export function RemovedVersions({
  org,
  database,
  admin,
}: {
  org: string;
  database: Database;
  /** May restore. */
  admin: boolean;
}) {
  const removed = useQuery(removedVersionsQuery(org, database.slug));
  return (
    <Card id="removed-versions" className="scroll-mt-4">
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle>Removed versions</CardTitle>
          <p className="text-sm text-muted-foreground">
            Hidden from the history, diffs and imports. An admin can restore one for 30 days after
            its removal; then it is deleted for good.
          </p>
        </div>
      </CardHeader>
      {removed.isPending ? (
        <div aria-hidden className="divide-y divide-border">
          <div className="flex items-center gap-3 px-4 py-3">
            <Skeleton className="h-4 w-8" />
            <div className="grid flex-1 gap-1.5">
              <Skeleton className="h-4 w-48" />
              <Skeleton className="h-3 w-64" />
            </div>
          </div>
        </div>
      ) : removed.isError ? (
        <div className="p-4">
          <Alert
            variant="destructive"
            title="Could not load the removed versions"
            action={
              <Button size="sm" loading={removed.isFetching} onClick={() => void removed.refetch()}>
                Retry
              </Button>
            }
          >
            {removed.error.message}
          </Alert>
        </div>
      ) : removed.data.length === 0 ? (
        <EmptyState
          className="py-8"
          icon={<EyeClosedIcon />}
          title="No removed versions"
          description={
            admin
              ? "To remove one, open it and choose Remove version… in its ⋯ menu."
              : "Admins can remove a version from its page; it would be listed here."
          }
        />
      ) : (
        <ul aria-label="Removed versions" className="divide-y divide-border">
          {removed.data.map((item) => (
            <Row key={item.version.id} org={org} database={database} item={item} admin={admin} />
          ))}
        </ul>
      )}
    </Card>
  );
}

function Row({
  org,
  database,
  item,
  admin,
}: {
  org: string;
  database: Database;
  item: RemovedVersion;
  admin: boolean;
}) {
  const restore = useRestoreVersion(org, database.slug);
  const { version } = item;
  const expired = !item.restorable;
  return (
    <li className="flex items-start gap-3 px-4 py-3">
      <span className="mt-px w-10 shrink-0 font-mono text-base font-semibold text-muted-foreground tabular-nums line-through decoration-faint-foreground">
        v{version.number}
      </span>
      <div className="grid min-w-0 flex-1 gap-1">
        <p className="flex min-w-0 items-center gap-2">
          <span
            className={
              version.message
                ? "truncate text-base text-foreground"
                : "text-base text-muted-foreground"
            }
          >
            {version.message ?? "No message"}
          </span>
          <OriginBadge origin={version.origin} />
        </p>
        <p className="text-sm text-muted-foreground">
          Removed by {item.removed_by?.name ?? "a deleted account"}{" "}
          <span title={formatDateTime(item.removed_at)}>{timeAgo(item.removed_at)}</span>
        </p>
        <blockquote className="border-l-2 border-border-strong pl-2 text-sm text-foreground">
          {item.reason}
        </blockquote>
        <p className="text-xs text-faint-foreground">
          {expired
            ? "No longer restorable: deleted for good within the hour."
            : `Restorable until ${formatDate(item.restorable_until)}`}
        </p>
      </div>
      {admin && !expired && (
        <Button
          size="sm"
          loading={restore.isPending}
          aria-label={`Restore v${version.number}`}
          onClick={() =>
            restore.mutate(version.number, {
              onSuccess: () =>
                toast.success(`v${version.number} restored`, {
                  description: "It is back in the history, diffs and imports.",
                }),
              onError: (error) =>
                toast.error(`Could not restore v${version.number}`, {
                  description: error instanceof ApiError ? error.message : "Check your connection.",
                }),
            })
          }
        >
          <UndoIcon />
          Restore
        </Button>
      )}
    </li>
  );
}
