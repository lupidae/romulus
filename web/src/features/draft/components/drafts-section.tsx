import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { GitBranchIcon, NavArrowRightIcon, PlusIcon } from "@/components/icons";
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
}: {
  org: string;
  db: string;
  variant?: "primary" | "secondary";
}) {
  const create = useCreateDraft(org, db);
  const navigate = useNavigate();
  return (
    <Button
      variant={variant}
      loading={create.isPending}
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
      New draft
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
  /** Off when the page has its own "New draft" button. */
  withNewButton?: boolean;
}) {
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  const drafts = useQuery(draftsQuery(org, db));
  const writer = canWrite(orgData.role);
  const id = React.useId();
  const count = drafts.data?.length ?? 0;

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

function DraftRow({ org, db, draft }: { org: string; db: string; draft: Draft }) {
  const people = draft.present.map((p) => ({
    id: p.user_id,
    name: p.name,
    detail: p.editing ? "editing" : "watching",
  }));
  const origin = draft.base_version ? `From v${draft.base_version}` : "From scratch";
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
          {draft.stale && <Badge variant="warning">v{draft.latest_version} is out</Badge>}
        </p>
        <p className="truncate text-sm text-muted-foreground">
          {people.length > 0 ? (
            <span className="text-foreground">{presenceLine(draft.present)}</span>
          ) : (
            <>
              {origin}
              {draft.created_by && ` · ${draft.created_by.name}`} · edited{" "}
              {fromNow(draft.updated_at)}
            </>
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
