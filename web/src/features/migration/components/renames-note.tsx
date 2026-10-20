import { useQuery } from "@tanstack/react-query";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";

import type { MigrationChoice, MigrationQuery, RenameDecisions } from "../queries";

export interface RenamesNoteProps {
  /** The migration's query, as MigrationView takes it. */
  query: (choice: MigrationChoice) => MigrationQuery;
  /** The renames switched on screen, over the stored ones. */
  decisions?: RenameDecisions;
  /** Shows the migration. */
  onShow: () => void;
  className?: string;
}

const NONE: RenameDecisions = {};

/**
 * Above changes compared by name, where a renamed column reads as removed
 * and added: how many of them the migration runs as renames, and the way
 * to it. Nothing until it knows, nothing when there are none.
 */
export function RenamesNote({ query, decisions = NONE, onShow, className }: RenamesNoteProps) {
  const migration = useQuery(query({ format: "plain", concurrently: false, renames: decisions }));
  const count = migration.data?.renames.filter((c) => c.applied && !c.follows).length ?? 0;
  if (count === 0) return null;
  return (
    <div
      className={cn(
        "flex shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-border bg-muted/60 px-3 py-1.5 text-sm text-muted-foreground",
        className,
      )}
    >
      <p className="min-w-0 flex-1">
        {count === 1
          ? "A rename below reads as removed and added; the migration keeps its data."
          : `${count} renames below read as removed and added; the migration keeps their data.`}
      </p>
      <Button size="sm" variant="link" className="h-auto px-0" onClick={onShow}>
        See the migration
      </Button>
    </div>
  );
}
