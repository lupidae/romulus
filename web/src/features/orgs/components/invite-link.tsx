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
      <CopyField value={url} label="Invitation link" autoCopy={autoCopy} />
      <p className="text-sm text-muted-foreground">
        Joins as {roleLabel[invitation.role].toLowerCase()}. Works once, only for{" "}
        <span className="font-medium text-foreground">{invitation.email}</span>, until{" "}
        {formatDate(invitation.expires_at)}.
      </p>
    </div>
  );
}
