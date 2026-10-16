import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import {
  CheckCircleIcon,
  GitBranchIcon,
  NavArrowRightIcon,
  PlusIcon,
  SendDiagonalIcon,
} from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { AvatarStack } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { orgQuery } from "@/features/orgs/queries";
import { canWrite } from "@/features/orgs/roles";
import { fromNow } from "@/lib/time";

import { useCreateDraft } from "../mutations";
import { presenceLine } from "../presence";
import { type Draft, draftsQuery } from "../queries";

/**
 * "New draft": one click, no dialog. The draft starts from the latest
 * version and the editor opens straight away.
 */
export function NewDraftButton({
  org,
  db,
  variant = "secondary",
  compact = false,
}: {
  org: string;
  db: string;
  variant?: "primary" | "secondary";
  /** Icon only below `sm` (a page header). */
  compact?: boolean;
}) {
  const create = useCreateDraft(org, db);
  const navigate = useNavigate();
  return (
    <Button
      variant={variant}
      loading={create.isPending}
      aria-label={compact ? "New draft" : undefined}
      className={compact ? "max-sm:px-2.5" : undefined}
      onClick={() =>
        create.mutate(undefined, {
          onSuccess: (draft) =>
            void navigate({
              to: "/$org/$db/drafts/$draft",
              params: { org, db, draft: draft.id },
            }),
          onError: (error) =>
            toast.error("Could not start a draft", {
              description: error instanceof ApiError ? error.message : "Check your connection.",
            }),
        })
      }
    >
      <PlusIcon />
      <span className={compact ? "max-sm:hidden" : undefined}>New draft</span>
    </Button>
  );
}

/**
 * The database page's open drafts: what each is called, where it started,
 * who is in it right now. Polled, so people come and go without a reload.
 */
export function DraftsSection({
  org,
  db,
  withNewButton = true,
}: {
  org: string;
  db: string;
  /**
   * Off when the page has its own "New draft" button: the section then
   * shows nothing while there are no drafts.
   */
  withNewButton?: boolean;
}) {
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  const drafts = useQuery(draftsQuery(org, db));
  const writer = canWrite(orgData.role);
  const id = React.useId();
  const count = drafts.data?.length ?? 0;
  if (!withNewButton && drafts.isSuccess && count === 0) return null;

  return (
    <section aria-labelledby={id} className="grid gap-3">
      <div className="flex min-h-8 items-center justify-between gap-3">
        <h2 id={id} className="flex items-center gap-2 text-base font-semibold text-foreground">
          Drafts
          {count > 0 && (
            <span className="rounded-sm bg-muted px-1 text-xs font-medium text-muted-foreground tabular">
              {count}
            </span>
          )}
        </h2>
        {writer && withNewButton && count > 0 && <NewDraftButton org={org} db={db} />}
      </div>
      {drafts.isPending ? (
        <Card className="divide-y divide-border p-0" aria-busy aria-label="Loading drafts">
          {[56, 40].map((w) => (
            <div key={w} className="flex items-center gap-3 px-4 py-3">
              <Skeleton className="size-4 rounded-xs" />
              <div className="grid flex-1 gap-1.5">
                <Skeleton className="h-3.5" style={{ width: `${w}%` }} />
                <Skeleton className="h-3 w-40" />
              </div>
            </div>
          ))}
        </Card>
      ) : drafts.isError ? (
        <Alert
          variant="destructive"
          title="Could not load the drafts"
          action={
            <Button size="sm" onClick={() => void drafts.refetch()}>
              Retry
            </Button>
          }
        >
          Check your connection and try again.
        </Alert>
      ) : count === 0 ? (
        <Card className="p-0">
          <EmptyState
            className="py-10"
            icon={<GitBranchIcon />}
            title="No drafts open"
            description={
              writer
                ? "A draft is where the next version is designed together: SQL and graph, live."
                : "Drafts appear here when a developer starts one. You can follow them live."
            }
            action={writer && <NewDraftButton org={org} db={db} variant="primary" />}
          />
        </Card>
      ) : (
        <Card className="p-0">
          <ul className="divide-y divide-border">
            {drafts.data.map((draft) => (
              <li key={draft.id}>
                <DraftRow org={org} db={db} draft={draft} />
              </li>
            ))}
          </ul>
        </Card>
      )}
    </section>
  );
}

/** "Edited just now", "Edited 5 minutes ago", "Edited yesterday". */
function edited(iso: string): string {
  return Date.now() - new Date(iso).getTime() < 60_000
    ? "Edited just now"
    : `Edited ${fromNow(iso)}`;
}

function DraftRow({ org, db, draft }: { org: string; db: string; draft: Draft }) {
  const people = draft.present.map((p) => ({
    id: p.user_id,
    name: p.name,
    detail: p.editing ? "editing" : "watching",
  }));
  return (
    <Link
      to="/$org/$db/drafts/$draft"
      params={{ org, db, draft: draft.id }}
      className="group flex items-center gap-3 px-4 py-3 outline-none first:rounded-t-[inherit] last:rounded-b-[inherit] hover:bg-accent/50 focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset"
    >
      <GitBranchIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
      <div className="grid min-w-0 flex-1 gap-0.5">
        <p className="flex min-w-0 items-center gap-2">
          <span className="truncate text-base font-medium text-foreground">{draft.name}</span>
          {draft.stale ? (
            <Badge variant="warning">
              {draft.base_removed
                ? `v${draft.base_version} removed`
                : `v${draft.latest_version} is out`}
            </Badge>
          ) : (
            draft.base_version && <Badge variant="outline">from v{draft.base_version}</Badge>
          )}
        </p>
        <p className="flex min-w-0 items-center gap-1.5 text-sm">
          <ReviewState review={draft.review} />
          {people.length > 0 ? (
            <span className="truncate text-foreground">{presenceLine(draft.present)}</span>
          ) : (
            <span className="truncate text-muted-foreground">{edited(draft.updated_at)}</span>
          )}
        </p>
      </div>
      {people.length > 0 && <AvatarStack people={people} size="sm" max={4} />}
      <NavArrowRightIcon
        aria-hidden
        className="size-4 shrink-0 text-faint-foreground transition-transform group-hover:translate-x-0.5"
      />
    </Link>
  );
}

/** "Review requested · 1 of 2 approvals ·", ahead of the row's second line. */
function ReviewState({ review }: { review: Draft["review"] }) {
  const parts: React.ReactNode[] = [];
  if (review.requested && review.approvals < review.required)
    parts.push(
      <span key="requested" className="flex shrink-0 items-center gap-1 font-medium text-info">
        <SendDiagonalIcon aria-hidden className="size-3.5" />
        Review requested
      </span>,
    );
  if (review.required > 0)
    parts.push(
      review.approvals >= review.required ? (
        <span key="approvals" className="flex shrink-0 items-center gap-1 font-medium text-success">
          <CheckCircleIcon aria-hidden className="size-3.5" />
          Approved
        </span>
      ) : (
        <span key="approvals" className="shrink-0 text-muted-foreground tabular-nums">
          {review.approvals} of {review.required} approvals
        </span>
      ),
    );
  if (parts.length === 0) return null;
  return (
    <>
      {parts.map((part, i) => (
        <React.Fragment key={i}>
          {i > 0 && (
            <span aria-hidden className="text-faint-foreground">
              ·
            </span>
          )}
          {part}
        </React.Fragment>
      ))}
      <span aria-hidden className="text-faint-foreground">
        ·
      </span>
    </>
  );
}

/**
 * The database header's word on drafts: how many are open, and who is in
 * them, linking to where they are listed. Nothing when none is open.
 */
export function OpenDrafts({ org, db }: { org: string; db: string }) {
  const drafts = useQuery(draftsQuery(org, db));
  const list = drafts.data ?? [];
  if (list.length === 0) return null;
  const people = new Map<string, { id: string; name: string; detail: string }>();
  for (const draft of list)
    for (const p of draft.present)
      if (!people.has(p.user_id) || p.editing)
        people.set(p.user_id, {
          id: p.user_id,
          name: p.name,
          detail: `${p.editing ? "editing" : "watching"} ${draft.name}`,
        });
  const label = list.length === 1 ? "1 open draft" : `${list.length} open drafts`;
  return (
    <Button variant="ghost" asChild className="gap-2 max-sm:px-2">
      <Link to="/$org/$db/history" params={{ org, db }}>
        {people.size > 0 && <AvatarStack people={[...people.values()]} size="sm" max={3} />}
        <GitBranchIcon className="sm:hidden" />
        <span className="max-sm:sr-only">{label}</span>
      </Link>
    </Button>
  );
}
