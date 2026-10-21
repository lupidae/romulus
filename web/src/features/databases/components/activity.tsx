import { useInfiniteQuery } from "@tanstack/react-query";

import {
  ActivityIcon,
  ChatLinesIcon,
  CheckCircleIcon,
  ClockIcon,
  CreditCardIcon,
  DatabaseIcon,
  DatabaseScriptIcon,
  EyeClosedIcon,
  FlaskIcon,
  GroupIcon,
  RefreshDoubleIcon,
  SettingsIcon,
  ShieldCheckIcon,
  SparksIcon,
  TrashIcon,
  UndoIcon,
  WarningTriangleIcon,
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
            publishing needs, moves to another Postgres version, comments deleted by someone other
            than their author, and playgrounds: created, reset, migrated, deleted.
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
  comment_deleted: ChatLinesIcon,
  playground_created: FlaskIcon,
  playground_deleted: TrashIcon,
  playground_reset: RefreshDoubleIcon,
  playground_migration_run: DatabaseScriptIcon,
  playground_restarted: WarningTriangleIcon,
  passing_test_required_changed: SettingsIcon,
  migration_test_acknowledged: ShieldCheckIcon,
  passing_test_overridden: ShieldCheckIcon,
  agent_action: SparksIcon,
  // Billing's (organization-level, kept out of a database's activity by the API).
  terms_accepted: CreditCardIcon,
  subscription_changed: CreditCardIcon,
  payment_failed: WarningTriangleIcon,
  payment_succeeded: CreditCardIcon,
  plan_downgraded: WarningTriangleIcon,
  playground_hours_used_up: ClockIcon,
  subscription_duplicate_cancelled: CreditCardIcon,
  // Kept out of a database's activity by the API: another database's purge.
  database_purged: TrashIcon,
  admin_appointed: GroupIcon,
  contract_withdrawn: CreditCardIcon,
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
    case "passing_test_required_changed":
      return details.to === 1
        ? `${who} made publishing wait for a passing migration test`
        : `${who} let drafts publish without a passing migration test`;
    case "migration_test_acknowledged":
      return `${who} acknowledged the data loss a migration test found`;
    case "passing_test_overridden":
      return `${who} published ${v} without a passing migration test (tests could not run)`;
    case "major_changed":
      return event.version_number == null
        ? `${who} moved the database from Postgres ${details.from ?? "?"} to ${details.to ?? "?"}`
        : `${who} moved the database from Postgres ${details.from ?? "?"} to ${details.to ?? "?"}, after ${v} compiled there`;
    case "comment_deleted": {
      const whose = names((details.authors ?? []).map((name) => ({ name })));
      return details.comments && details.comments > 1
        ? `${who} deleted a thread of ${details.comments} comments (${whose || "others"})`
        : `${who} deleted ${whose ? `${whose}'s` : "a"} comment`;
    }
    case "playground_created":
      return `${who} created a playground of ${v}`;
    case "playground_deleted":
      return `${who} deleted a playground of ${v}`;
    case "playground_reset":
      return `${who} reset the rows of a playground of ${v}`;
    case "playground_migration_run":
      return `${who} ran a migration on a playground of ${v}${details.outcome ? `: ${details.outcome}` : ""}`;
    case "playground_restarted":
      return `A playground of ${v} restarted to undo a role changed from inside it`;
    case "agent_action": {
      const did = details.outcome ?? `used ${details.tool ?? "a tool"}`;
      return details.app_name
        ? `${who}'s agent ${did}, through the connected app “${details.app_name}”`
        : `${who}'s agent ${did}, with the access token “${details.token_name ?? "?"}”`;
    }
    case "terms_accepted":
      return `${who} accepted the Terms of Service to subscribe`;
    case "subscription_changed":
      return "The subscription changed";
    case "payment_failed":
      return "A payment failed";
    case "payment_succeeded":
      return "A payment went through";
    case "plan_downgraded":
      return "The organization moved to the Free plan after an unpaid invoice";
    case "playground_hours_used_up":
      return "The organization used its playground hours for the month";
    case "subscription_duplicate_cancelled":
      return "A second subscription was cancelled and refunded";
    case "database_purged":
      return `The database “${details.database_name ?? details.database_slug ?? "?"}” was deleted for good, 30 days after its deletion`;
    case "admin_appointed":
      return `${details.member?.name ?? "A member"} was made an admin of the organization`;
    case "contract_withdrawn":
      return `${who} withdrew from the paid plan within 14 days: it was cancelled and the days not used refunded`;
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
            {details.draft_name
              ? `${event.action === "comment_deleted" ? "In" : event.action.startsWith("playground_") || event.action === "migration_test_acknowledged" || event.action === "agent_action" ? "For" : "From"} the draft “${details.draft_name}”`
              : details.message}
          </p>
        )}
        {details.excerpt && (
          <p className="line-clamp-2 text-sm text-muted-foreground">“{details.excerpt}”</p>
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
