import { MailOutIcon } from "@/components/icons";
import { CopyField } from "@/components/ui/copy-field";
import { formatDate } from "@/lib/time";

import type { CreatedInvitation } from "../queries";
import { roleLabel } from "../roles";

/** A freshly made invitation link, copied for the admin, with what it does. */
export function InviteLink({
  created,
  autoCopy = true,
}: {
  created: CreatedInvitation;
  autoCopy?: boolean;
}) {
  const { invitation, url } = created;
  return (
    <div className="grid gap-2">
      {created.emailed && (
        <p className="flex items-start gap-1.5 text-sm text-success">
          <MailOutIcon className="mt-0.5 size-4 shrink-0" />
          <span>
            Emailed to <span className="font-medium">{invitation.email}</span>. You can also send
            the link yourself.
          </span>
        </p>
      )}
      <CopyField value={url} label="Invitation link" autoCopy={autoCopy && !created.emailed} />
      <p className="text-sm text-muted-foreground">
        Joins as {roleLabel[invitation.role].toLowerCase()}. Works once, only for{" "}
        <span className="font-medium text-foreground">{invitation.email}</span>, until{" "}
        {formatDate(invitation.expires_at)}.
      </p>
    </div>
  );
}
