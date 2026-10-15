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
 * once). No email leaves romulus; the admin sends the link themselves.
 */
export function InviteDialog({ open, onOpenChange, slug, orgName, pending }: InviteDialogProps) {
  const [created, setCreated] = React.useState<CreatedInvitation>();

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
                Send it to {created.invitation.email} however you like: chat, email, a ticket.
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
                You get a link to send. It works once, for this email address only, for 7 days.
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
