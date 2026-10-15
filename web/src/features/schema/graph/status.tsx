import { Badge } from "@/components/ui/badge";

import { type ChangeStatus, STATUS_LABEL } from "../diff";

const VARIANT = { added: "success", removed: "destructive", changed: "warning" } as const;

/** A diff status as a word: color is never the only signal. */
export function StatusBadge({ status, className }: { status: ChangeStatus; className?: string }) {
  return (
    <Badge variant={VARIANT[status]} className={className}>
      {STATUS_LABEL[status]}
    </Badge>
  );
}
