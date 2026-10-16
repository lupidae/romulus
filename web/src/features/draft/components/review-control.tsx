import { useQueryClient } from "@tanstack/react-query";

import { ApiError } from "@/api/client";
import { CheckCircleIcon, SendDiagonalIcon, ShieldCheckIcon } from "@/components/icons";
import { Avatar } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { toast } from "@/components/ui/toaster";
import type { Role } from "@/features/orgs/queries";
import { fromNow } from "@/lib/time";
import { cn } from "@/lib/utils";

import { useApproveDraft, useReviewRequest, useWithdrawApproval } from "../mutations";
import { draftKeys } from "../queries";
import { approvalsLabel, approved, names, type Review, sha256Hex, standing } from "../review";

interface ReviewControlProps {
  org: string;
  db: string;
  draftId: string;
  review: Review;
  meId: string;
  /** This connection's role (null until the server says). */
  role: Role | null;
  /** The draft is open (not being published or updated). */
  open: boolean;
  /** The SQL now: an approval is of exactly this text. */
  sql: string;
}

/**
 * The editor's word on approvals: "1 of 2 approvals" with who approved, and
 * what this person can do about it (approve, take it back, ask for a
 * review). Approve sits in the bar too for whoever can give one: it is
 * a reviewer's one action in a draft.
 */
export function ReviewControl(props: ReviewControlProps) {
  const { review, meId, role, open } = props;
  const mine = standing(review, meId, role);
  const approve = useApprove(props);
  return (
    <div className="flex items-center gap-1">
      <ReviewPopover {...props} approve={approve} />
      {mine === "can_approve" && open && (
        <Button
          variant={role === "reviewer" ? "primary" : "secondary"}
          loading={approve.isPending}
          onClick={approve.run}
          aria-label="Approve this version of the draft"
          // A narrow bar keeps a writer's room for Publish: Approve is in the popover too.
          className={role === "reviewer" ? undefined : "max-sm:hidden"}
        >
          <CheckCircleIcon />
          <span className="max-sm:sr-only">Approve</span>
        </Button>
      )}
    </div>
  );
}

function useApprove({ org, db, draftId, sql }: ReviewControlProps) {
  const mutation = useApproveDraft(org, db, draftId);
  const queryClient = useQueryClient();
  const run = async () => {
    if (mutation.isPending) return;
    try {
      const after = await mutation.mutateAsync(await sha256Hex(sql));
      const left = Math.max(0, after.required - after.approvals);
      toast.success("Approved", {
        description:
          left === 0
            ? "The draft has the approvals it needs to be published."
            : `${left} more approval${left === 1 ? "" : "s"} needed before it can be published.`,
      });
    } catch (error) {
      if (error instanceof ApiError && error.code === "draft_changed") {
        void queryClient.invalidateQueries({ queryKey: draftKeys.preview(org, db, draftId) });
        toast.warning("The SQL just changed", {
          description:
            "Look at the latest edit, then approve again: an approval is of one version.",
        });
      } else {
        toast.error("Could not approve", {
          description: error instanceof ApiError ? error.message : "Check your connection.",
        });
      }
    }
  };
  return { run: () => void run(), isPending: mutation.isPending };
}

function ReviewPopover({
  org,
  db,
  draftId,
  review,
  meId,
  role,
  open,
  approve,
}: ReviewControlProps & { approve: ReturnType<typeof useApprove> }) {
  const withdraw = useWithdrawApproval(org, db, draftId);
  const request = useReviewRequest(org, db, draftId);
  const done = approved(review);
  const mine = standing(review, meId, role);
  const label = approvalsLabel(review);
  const left = Math.max(0, review.required - review.approvals.length);
  const writer = role === "admin" || role === "developer";

  const failed = (what: string) => (error: unknown) =>
    toast.error(what, {
      description: error instanceof ApiError ? error.message : "Check your connection.",
    });

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          className={cn("gap-1.5 max-sm:px-2", done && "text-success")}
          aria-label={`${label}. Show who approved`}
          data-testid="approvals"
        >
          {done ? <CheckCircleIcon /> : <ShieldCheckIcon className="text-muted-foreground" />}
          <span className="tabular-nums max-sm:hidden">{label}</span>
          <span className="tabular-nums sm:hidden">
            {Math.min(review.approvals.length, review.required)}/{review.required}
          </span>
          {review.approvals.length > 0 && (
            <span aria-hidden className="ml-0.5 flex -space-x-1 max-md:hidden">
              {review.approvals.slice(0, 3).map((a) => (
                <span key={a.user_id} className="rounded-full ring-2 ring-card">
                  <Avatar name={a.name} id={a.user_id} size="sm" />
                </span>
              ))}
            </span>
          )}
        </Button>
      </PopoverTrigger>
      <PopoverContent align="end" className="grid w-80 gap-3 p-0">
        <div className="grid gap-1 px-3 pt-3">
          <div className="flex items-center justify-between gap-2">
            <h2 className="text-base font-semibold text-foreground">Approvals</h2>
            {done ? (
              <Badge variant="success">
                <CheckCircleIcon aria-hidden />
                Ready to publish
              </Badge>
            ) : (
              <Badge variant="warning">{left} more needed</Badge>
            )}
          </div>
          <p className="text-sm text-muted-foreground">
            Publishing needs {review.required} approval{review.required === 1 ? "" : "s"} from
            members who did not edit the SQL. Any edit resets them.
          </p>
        </div>

        <div className="border-y border-border px-3 py-2">
          {review.approvals.length === 0 ? (
            <p className="py-1 text-sm text-muted-foreground">No approval of this version yet.</p>
          ) : (
            <ul aria-label="Approved by" className="grid gap-2">
              {review.approvals.map((a) => (
                <li key={a.user_id} className="flex items-center gap-2">
                  <Avatar name={a.name} id={a.user_id} size="md" />
                  <span className="min-w-0 flex-1 truncate text-sm text-foreground">
                    {a.user_id === meId ? `${a.name} (you)` : a.name}
                  </span>
                  <span className="shrink-0 text-xs text-muted-foreground">{fromNow(a.at)}</span>
                </li>
              ))}
            </ul>
          )}
          {review.outdated.length > 0 && (
            <p className="mt-2 text-sm text-muted-foreground">
              Reset by an edit since: {names(review.outdated)}.
            </p>
          )}
        </div>

        {review.requested && (
          <p className="flex items-center gap-2 px-3 text-sm text-foreground">
            <SendDiagonalIcon aria-hidden className="size-4 shrink-0 text-info" />
            <span className="min-w-0">
              Review requested by {review.requested.name ?? "a deleted account"},{" "}
              {fromNow(review.requested.at)}
            </span>
          </p>
        )}

        <div className="flex flex-wrap items-center justify-end gap-2 px-3 pb-3">
          {mine === "editor" && (
            <p className="mr-auto text-sm text-muted-foreground">
              You edited the SQL: someone else approves it.
            </p>
          )}
          {mine === "approved" && (
            <>
              <p className="mr-auto flex items-center gap-1.5 text-sm text-success">
                <CheckCircleIcon aria-hidden className="size-4" />
                You approved this version
              </p>
              <Button
                size="sm"
                variant="ghost"
                loading={withdraw.isPending}
                onClick={() =>
                  withdraw.mutate(undefined, {
                    onSuccess: () => toast.success("Approval withdrawn"),
                    onError: failed("Could not withdraw the approval"),
                  })
                }
              >
                Withdraw
              </Button>
            </>
          )}
          {mine === "can_approve" && open && (
            <Button size="sm" variant="primary" loading={approve.isPending} onClick={approve.run}>
              <CheckCircleIcon />
              Approve this version
            </Button>
          )}
          {writer && !done && open && (
            <Button
              size="sm"
              variant={mine === "editor" && !review.requested ? "primary" : "secondary"}
              loading={request.isPending}
              onClick={() =>
                request.mutate(!review.requested, {
                  onSuccess: () =>
                    toast.success(
                      review.requested ? "Review request withdrawn" : "Review requested",
                      {
                        description: review.requested
                          ? undefined
                          : "The drafts list shows it waits for a review.",
                      },
                    ),
                  onError: failed("Could not change the review request"),
                })
              }
            >
              {review.requested ? (
                "Withdraw request"
              ) : (
                <>
                  <SendDiagonalIcon />
                  Request review
                </>
              )}
            </Button>
          )}
        </div>
      </PopoverContent>
    </Popover>
  );
}
