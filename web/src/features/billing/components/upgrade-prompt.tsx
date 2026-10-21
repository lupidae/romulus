import { useQuery } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";

import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { orgQuery } from "@/features/orgs/queries";
import { canManageMembers } from "@/features/orgs/roles";

import { capTitles, type PlanLimit, planLimitOf } from "../plan-limit";
import { closeUpgrade, useShownUpgrade } from "../upgrade";

/** Whether the viewer may change `org`'s plan (admins); `undefined` while unknown. */
function useCanManage(org: string | undefined): boolean | undefined {
  const query = useQuery({ ...orgQuery(org ?? ""), enabled: !!org });
  return query.data ? canManageMembers(query.data.role) : undefined;
}

/** The one way out of a refusal: the plans, on the billing page. */
function SeePlans({ org, onGo }: { org: string; onGo?: () => void }) {
  return (
    <Button size="sm" asChild>
      <Link to="/$org/settings/billing" params={{ org }} onClick={onGo}>
        See plans
      </Link>
    </Button>
  );
}

function whoUpgrades(canManage: boolean | undefined): string | null {
  return canManage === false ? "Only an admin of the organization can change its plan." : null;
}

/**
 * A refusal by the plan, where it happened (a form, a card): what was
 * refused, the API's sentence (the cap, the plan that lifts it), one button.
 * Renders nothing for any other error.
 */
export function PlanLimitNotice({
  error,
  org,
  className,
}: {
  error: unknown;
  org: string;
  className?: string;
}) {
  const limit = planLimitOf(error);
  const canManage = useCanManage(org);
  if (!limit) return null;
  const who = whoUpgrades(canManage);
  // The button under the sentence, not beside it: the sentence keeps the
  // width it needs in a dialog or on a phone.
  return (
    <Alert variant="info" title={capTitles[limit.cap]} className={className}>
      {limit.message}
      {who && <span className="mt-1 block">{who}</span>}
      <span className="mt-2 block">
        <SeePlans org={org} />
      </span>
    </Alert>
  );
}

/** The upgrade dialog `showUpgrade(error)` opens, for refusals with no form to hold them. */
export function UpgradeDialogHost() {
  const limit = useShownUpgrade();
  const { org } = useParams({ strict: false });
  return (
    <Dialog open={limit !== null && !!org} onOpenChange={(open) => !open && closeUpgrade()}>
      <DialogContent>{limit && org && <UpgradeDialogBody limit={limit} org={org} />}</DialogContent>
    </Dialog>
  );
}

function UpgradeDialogBody({ limit, org }: { limit: PlanLimit; org: string }) {
  const who = whoUpgrades(useCanManage(org));
  return (
    <>
      <DialogHeader>
        <DialogTitle>{capTitles[limit.cap]}</DialogTitle>
        <DialogDescription>{limit.message}</DialogDescription>
      </DialogHeader>
      {who && <p className="text-sm text-muted-foreground">{who}</p>}
      <DialogFooter>
        <SeePlans org={org} onGo={closeUpgrade} />
      </DialogFooter>
    </>
  );
}
