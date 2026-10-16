import { useInfiniteQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import * as React from "react";

import { ClockIcon, GitCompareIcon, NavArrowDownIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuSeparator,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { originLabel, versionsQuery } from "@/features/databases/queries";
import { timeAgo } from "@/lib/time";

interface CompareWithProps {
  org: string;
  db: string;
  /** The version shown. */
  number: number;
  latest: number;
  /** The version before it (removed ones skipped); none for the first. */
  previous?: number;
}

/**
 * "Compare ▾" on a version: with the one before it, the latest, or any recent
 * one. The older of the two goes on the left.
 */
export function CompareWith({ org, db, number, latest, previous }: CompareWithProps) {
  const [open, setOpen] = React.useState(false);
  const versions = useInfiniteQuery({ ...versionsQuery(org, db), enabled: open });
  const recent = (versions.data?.pages[0]?.versions ?? []).filter((v) => v.number !== number);
  // The only version: nothing to compare it with.
  if (previous === undefined && number >= latest) return null;
  const range = (other: number) => ({
    from: Math.min(number, other),
    to: Math.max(number, other),
  });
  return (
    <Menu open={open} onOpenChange={setOpen}>
      <MenuTrigger asChild>
        <Button variant="secondary" size="sm">
          <GitCompareIcon />
          Compare
          <NavArrowDownIcon className="text-muted-foreground" />
        </Button>
      </MenuTrigger>
      <MenuContent align="end" className="w-80">
        <MenuLabel>Compare v{number} with</MenuLabel>
        {previous !== undefined && (
          <MenuItem asChild>
            <Link to="/$org/$db/diff/$range" params={{ org, db, range: range(previous) }}>
              <span className="w-9 shrink-0 font-mono text-sm font-medium tabular-nums">
                v{previous}
              </span>
              <span className="text-sm text-muted-foreground">The version before</span>
            </Link>
          </MenuItem>
        )}
        {number < latest && (
          <MenuItem asChild>
            <Link to="/$org/$db/diff/$range" params={{ org, db, range: range(latest) }}>
              <span className="w-9 shrink-0 font-mono text-sm font-medium tabular-nums">
                v{latest}
              </span>
              <span className="text-sm text-muted-foreground">The latest version</span>
            </Link>
          </MenuItem>
        )}
        <MenuSeparator />
        <MenuLabel>Recent versions</MenuLabel>
        {versions.isPending
          ? Array.from({ length: 4 }, (_, i) => (
              <div key={i} className="mx-1 h-8 animate-pulse rounded-md bg-muted/60" />
            ))
          : recent.slice(0, 10).map((v) => (
              <MenuItem key={v.id} asChild>
                <Link
                  to="/$org/$db/diff/$range"
                  params={{ org, db, range: range(v.number) }}
                  className="flex items-center gap-2"
                >
                  <span className="w-9 shrink-0 font-mono text-sm font-medium text-foreground tabular-nums">
                    v{v.number}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-sm text-muted-foreground">
                    {v.message ??
                      `${originLabel[v.origin]} by ${v.author?.name ?? "a deleted account"}`}
                  </span>
                  <span className="shrink-0 text-xs text-faint-foreground">
                    {timeAgo(v.created_at)}
                  </span>
                </Link>
              </MenuItem>
            ))}
        <MenuSeparator />
        <MenuItem asChild>
          <Link to="/$org/$db/history" params={{ org, db }}>
            <ClockIcon />
            Pick any two in the history
          </Link>
        </MenuItem>
      </MenuContent>
    </Menu>
  );
}
