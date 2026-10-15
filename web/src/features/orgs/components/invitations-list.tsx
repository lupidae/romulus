import * as React from "react";

import { ApiError } from "@/api/client";
import { MoreHorizIcon, RefreshDoubleIcon, TrashIcon } from "@/components/icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Menu,
  MenuContent,
  MenuItem,
  MenuSeparator,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Skeleton } from "@/components/ui/skeleton";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { formatDate, fromNow } from "@/lib/time";

import { useInvite, useRevokeInvitation } from "../mutations";
import type { CreatedInvitation, Invitation } from "../queries";
import { roleLabel } from "../roles";
import { InviteLink } from "./invite-link";

function failed(error: unknown) {
  toast.error(error instanceof ApiError ? error.message : "Something went wrong. Try again.");
}

/**
 * Pending invitations. Links are shown once, so the way to share one again
 * is a new link (which retires the old one); revoking asks first.
 */
export function InvitationsList({
  slug,
  orgName,
  invitations,
}: {
  slug: string;
  orgName: string;
  invitations: Invitation[];
}) {
  const invite = useInvite(slug);
  const revoke = useRevokeInvitation(slug);
  const [renewed, setRenewed] = React.useState<CreatedInvitation>();
  const [revoking, setRevoking] = React.useState<Invitation>();
  const [renewing, setRenewing] = React.useState<string>();

  const renew = (invitation: Invitation) => {
    setRenewing(invitation.id);
    invite.mutate(
      { email: invitation.email, role: invitation.role },
      {
        onSuccess: setRenewed,
        onError: failed,
        onSettled: () => setRenewing(undefined),
      },
    );
  };

  return (
    <>
      <Table>
        <TableHeader>
          <TableRow>
            <TableHead>Email</TableHead>
            <TableHead className="hidden sm:table-cell">Role</TableHead>
            <TableHead className="hidden sm:table-cell">Link</TableHead>
            <TableHead className="hidden md:table-cell">Invited by</TableHead>
            <TableHead className="w-12">
              <span className="sr-only">Actions</span>
            </TableHead>
          </TableRow>
        </TableHeader>
        <TableBody>
          {invitations.map((invitation) => (
            <TableRow key={invitation.id} aria-busy={renewing === invitation.id || undefined}>
              <TableCell className="w-full max-w-0">
                <div className="grid min-w-0">
                  <span className="truncate font-medium text-foreground">{invitation.email}</span>
                  {/* On phones the role and the link's state move under the address. */}
                  <span className="truncate text-muted-foreground sm:hidden">
                    {roleLabel[invitation.role]} ·{" "}
                    {invitation.expired ? "Expired" : `Expires ${fromNow(invitation.expires_at)}`}
                  </span>
                </div>
              </TableCell>
              <TableCell className="hidden sm:table-cell">
                <Badge variant={invitation.role === "admin" ? "primary" : "neutral"}>
                  {roleLabel[invitation.role]}
                </Badge>
              </TableCell>
              <TableCell className="hidden whitespace-nowrap sm:table-cell">
                {invitation.expired ? (
                  <Badge variant="warning">Expired</Badge>
                ) : (
                  <span className="text-muted-foreground">
                    Expires {fromNow(invitation.expires_at)}
                  </span>
                )}
              </TableCell>
              <TableCell className="hidden whitespace-nowrap text-muted-foreground md:table-cell">
                {invitation.invited_by?.name ?? "A former member"}
                <span className="text-faint-foreground">
                  {" "}
                  · {formatDate(invitation.created_at)}
                </span>
              </TableCell>
              <TableCell className="pr-2">
                <Menu>
                  <Tooltip content="More actions">
                    <MenuTrigger asChild>
                      <Button
                        variant="ghost"
                        size="icon"
                        loading={renewing === invitation.id}
                        aria-label={`More actions for the invitation of ${invitation.email}`}
                      >
                        <MoreHorizIcon />
                      </Button>
                    </MenuTrigger>
                  </Tooltip>
                  <MenuContent align="end" className="w-56">
                    <MenuItem onSelect={() => renew(invitation)}>
                      <RefreshDoubleIcon />
                      {invitation.expired ? "Renew link" : "Get a new link"}
                    </MenuItem>
                    <MenuSeparator />
                    <MenuItem variant="destructive" onSelect={() => setRevoking(invitation)}>
                      <TrashIcon />
                      Revoke invitation…
                    </MenuItem>
                  </MenuContent>
                </Menu>
              </TableCell>
            </TableRow>
          ))}
        </TableBody>
      </Table>

      <Dialog open={renewed !== undefined} onOpenChange={(open) => !open && setRenewed(undefined)}>
        <DialogContent>
          <DialogHeader>
            <DialogTitle>New link for {renewed?.invitation.email}</DialogTitle>
            <DialogDescription>The previous link no longer works.</DialogDescription>
          </DialogHeader>
          {renewed && <InviteLink created={renewed} />}
          <DialogFooter>
            <DialogClose asChild>
              <Button variant="primary">Done</Button>
            </DialogClose>
          </DialogFooter>
        </DialogContent>
      </Dialog>

      <ConfirmDialog
        open={revoking !== undefined}
        onOpenChange={(open) => !open && setRevoking(undefined)}
        title={`Revoke the invitation for ${revoking?.email ?? ""}?`}
        description={`The link stops working. You can invite them to ${orgName} again later.`}
        confirmLabel="Revoke invitation"
        destructive
        onConfirm={async () => {
          if (!revoking) return;
          try {
            await revoke.mutateAsync(revoking.id);
            toast.success(`Invitation for ${revoking.email} revoked`);
          } catch (error) {
            failed(error);
            throw error;
          }
        }}
      />
    </>
  );
}

export function InvitationsListSkeleton() {
  return (
    <Table aria-busy>
      <TableHeader>
        <TableRow>
          <TableHead>Email</TableHead>
          <TableHead className="hidden sm:table-cell">Role</TableHead>
          <TableHead className="hidden sm:table-cell">Link</TableHead>
          <TableHead className="hidden md:table-cell">Invited by</TableHead>
          <TableHead className="w-12">
            <span className="sr-only">Actions</span>
          </TableHead>
        </TableRow>
      </TableHeader>
      <TableBody>
        {[0, 1].map((i) => (
          <TableRow key={i}>
            <TableCell className="w-full max-w-0">
              <Skeleton className="h-3.5 w-48 max-w-full" />
            </TableCell>
            <TableCell className="hidden sm:table-cell">
              <Skeleton className="h-5 w-16" />
            </TableCell>
            <TableCell className="hidden sm:table-cell">
              <Skeleton className="h-3.5 w-24" />
            </TableCell>
            <TableCell className="hidden md:table-cell">
              <Skeleton className="h-3.5 w-28" />
            </TableCell>
            <TableCell />
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}
