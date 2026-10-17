import { useInfiniteQuery } from "@tanstack/react-query";

import {
  ActivityIcon,
  CheckCircleIcon,
  DatabaseIcon,
  EyeClosedIcon,
  SettingsIcon,
  ShieldCheckIcon,
  TrashIcon,
  UndoIcon,
} from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Card, CardHeader, CardTitle } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { formatDateTime, timeAgo } from "@/lib/time";

import { names } from "@/features/draft/review";

import { activityQuery, type AuditEvent } from "../queries";

/**
 * The database's audit log, newest first: versions removed, restored and
 * purged, drafts published without their approvals. Who, when, why.
 */
export function Activity({ org, db }: { org: string; db: string }) {
  const activity = useInfiniteQuery(activityQuery(org, db));
  const events = activity.data?.pages.flatMap((page) => page.events) ?? [];
  return (
    <Card id="activity" className="scroll-mt-4">
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle>Activity</CardTitle>
          <p className="text-sm text-muted-foreground">
            Removed and restored versions, drafts published and who approved them, the approvals
            publishing needs, and moves to another Postgres version.
          </p>
        </div>
      </CardHeader>
      {activity.isPending ? (
        <div aria-hidden className="divide-y divide-border">
          {[56, 40].map((w) => (
            <div key={w} className="flex items-center gap-3 px-4 py-3">
              <Skeleton className="size-4 rounded-full" />
              <div className="grid flex-1 gap-1.5">
                <Skeleton className="h-4" style={{ width: `${w}%` }} />
                <Skeleton className="h-3 w-24" />
              </div>
            </div>
          ))}
        </div>
      ) : activity.isError ? (
        <div className="p-4">
          <Alert
            variant="destructive"
            title="Could not load the activity"
            action={
              <Button
                size="sm"
                loading={activity.isFetching}
                onClick={() => void activity.refetch()}
              >
                Retry
              </Button>
            }
          >
            {activity.error.message}
          </Alert>
        </div>
      ) : events.length === 0 ? (
        <EmptyState
          className="py-8"
          icon={<ActivityIcon />}
          title="Nothing to account for yet"
          description="Removing or restoring a version, publishing a draft that needed approvals, and changing how many it needs are recorded here."
        />
      ) : (
        <>
          <ol aria-label="Activity" className="divide-y divide-border">
            {events.map((event) => (
              <Event key={event.id} event={event} />
            ))}
          </ol>
          {activity.hasNextPage && (
            <div className="border-t border-border px-4 py-3">
              <Button
                size="sm"
                variant="ghost"
                loading={activity.isFetchingNextPage}
                onClick={() => void activity.fetchNextPage()}
              >
                Show older
              </Button>
            </div>
          )}
        </>
      )}
    </Card>
  );
}

const icons = {
  version_removed: EyeClosedIcon,
  version_restored: UndoIcon,
  version_purged: TrashIcon,
  version_published: CheckCircleIcon,
  approvals_overridden: ShieldCheckIcon,
  approvals_required_changed: SettingsIcon,
  major_changed: DatabaseIcon,
} as const;

function approvalsCount(n: number | null | undefined): string {
  return n === 0 ? "none" : n === 1 ? "1 approval" : `${n ?? "?"} approvals`;
}

/** The event as a sentence: who did what to which version. */
function sentence(event: AuditEvent): string {
  const who = event.actor?.name ?? "A deleted account";
  const v = `v${event.version_number ?? "?"}`;
  const { details } = event;
  switch (event.action) {
    case "version_removed":
      return `${who} removed ${v}`;
    case "version_restored":
      return `${who} restored ${v}`;
    case "version_purged":
      return `${v} was deleted for good, 30 days after its removal`;
    case "version_published":
      return `${who} published ${v} with ${approvalsCount(details.approvals)} (${details.required ?? "?"} required)`;
    case "approvals_overridden":
      return `${who} published ${v} without its approvals (${details.approvals ?? 0} of ${details.required ?? "?"})`;
    case "approvals_required_changed":
      return `${who} changed the approvals publishing needs: ${approvalsCount(details.from)} → ${approvalsCount(details.to)}`;
    case "major_changed":
      return event.version_number == null
        ? `${who} moved the database from Postgres ${details.from ?? "?"} to ${details.to ?? "?"}`
        : `${who} moved the database from Postgres ${details.from ?? "?"} to ${details.to ?? "?"}, after ${v} compiled there`;
  }
}

function Event({ event }: { event: AuditEvent }) {
  const Icon = icons[event.action];
  const { details } = event;
  return (
    <li className="flex items-start gap-3 px-4 py-3">
      <Icon aria-hidden className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
      <div className="grid min-w-0 flex-1 gap-1">
        <p className="text-base text-foreground">{sentence(event)}</p>
        {(details.message || details.draft_name) && (
          <p className="truncate text-sm text-muted-foreground">
            {details.draft_name ? `From the draft “${details.draft_name}”` : details.message}
          </p>
        )}
        {details.approved_by && details.approved_by.length > 0 && (
          <p className="flex items-center gap-1.5 text-sm text-muted-foreground">
            <CheckCircleIcon aria-hidden className="size-3.5 shrink-0 text-success" />
            Approved by {names(details.approved_by)}
          </p>
        )}
        {event.reason && <p className="text-sm text-foreground">“{event.reason}”</p>}
      </div>
      <time
        dateTime={event.created_at}
        title={formatDateTime(event.created_at)}
        className="shrink-0 text-sm text-muted-foreground"
      >
        {timeAgo(event.created_at)}
      </time>
    </li>
  );
}
