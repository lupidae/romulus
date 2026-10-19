import { useQuery } from "@tanstack/react-query";
import * as React from "react";

import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";

import { instanceQuery } from "@/features/auth/queries";

import type { CreatedInvitation, Invitation } from "../queries";
import { InviteForm } from "./invite-form";
import { InviteLink } from "./invite-link";

interface InviteDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  slug: string;
  orgName: string;
  pending?: Invitation[];
}

/**
 * Invite someone: email and role, then the link to send them (copied at
 * once). When the server sends email it also mails the link; otherwise the
 * admin sends it themselves.
 */
export function InviteDialog({ open, onOpenChange, slug, orgName, pending }: InviteDialogProps) {
  const [created, setCreated] = React.useState<CreatedInvitation>();
  const mail = useQuery(instanceQuery).data?.mail === true;

  const change = (next: boolean) => {
    onOpenChange(next);
    if (!next) setCreated(undefined);
  };

  return (
    <Dialog open={open} onOpenChange={change}>
      <DialogContent>
        {created ? (
          <>
            <DialogHeader>
              <DialogTitle>Invitation link ready</DialogTitle>
              <DialogDescription>
                {created.emailed
                  ? `romulus emailed it to ${created.invitation.email}.`
                  : `Send it to ${created.invitation.email} however you like: chat, email, a ticket.`}
              </DialogDescription>
            </DialogHeader>
            <InviteLink created={created} />
            <DialogFooter>
              <Button onClick={() => setCreated(undefined)}>Invite someone else</Button>
              <DialogClose asChild>
                <Button variant="primary">Done</Button>
              </DialogClose>
            </DialogFooter>
          </>
        ) : (
          <>
            <DialogHeader>
              <DialogTitle>Invite to {orgName}</DialogTitle>
              <DialogDescription>
                {mail
                  ? "romulus emails them a link, and you get it too. It works once, for this email address only, for 7 days."
                  : "You get a link to send. It works once, for this email address only, for 7 days."}
              </DialogDescription>
            </DialogHeader>
            <InviteForm
              slug={slug}
              pending={pending}
              onCreated={setCreated}
              secondaryAction={
                <DialogClose asChild>
                  <Button>Cancel</Button>
                </DialogClose>
              }
            />
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
