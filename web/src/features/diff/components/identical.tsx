import type * as React from "react";

import { GitCompareIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";

interface IdenticalProps {
  from: number;
  to: number;
  onCompare: (from: number, to: number) => void;
  /** The way to the newer version ("Open v5"), a button. */
  open?: React.ReactNode;
}

/**
 * Two versions with the same schema (an upload of the same SQL, a draft that
 * only moved tables): say so, and offer the way on.
 */
export function Identical({ from, to, onCompare, open }: IdenticalProps) {
  const low = Math.min(from, to);
  const older = from !== to && low > 1 ? ([low - 1, low] as const) : undefined;
  return (
    <div className="flex h-full items-center justify-center overflow-y-auto px-4">
      <EmptyState
        icon={<GitCompareIcon />}
        title={
          from === to
            ? `v${from} compared with itself`
            : `No schema changes between v${from} and v${to}`
        }
        description={
          from === to
            ? "Pick another version on either side to see what changed."
            : `Both define the same tables, types and extensions. v${Math.max(from, to)} may differ only in its SQL text, its layout or its groups.`
        }
        action={
          <>
            {older && (
              <Button variant="primary" onClick={() => onCompare(...older)}>
                Compare v{older[0]} → v{older[1]}
              </Button>
            )}
            {open}
          </>
        }
      />
    </div>
  );
}
