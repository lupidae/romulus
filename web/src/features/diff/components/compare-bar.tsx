import * as React from "react";

import {
  ArrowRightIcon,
  CheckIcon,
  DataTransferBothIcon,
  NavArrowDownIcon,
  NavArrowLeftIcon,
  NavArrowRightIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuSeparator,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tooltip } from "@/components/ui/tooltip";
import { originLabel, type VersionSummary } from "@/features/databases/queries";
import { timeAgo } from "@/lib/time";
import { useShortcut } from "@/lib/use-shortcut";

interface CompareBarProps {
  from: number;
  to: number;
  /** The newest version's number: the pair cannot move past it. */
  latest: number;
  /** Recent versions, newest first, for the pickers (`undefined` while they load). */
  versions: VersionSummary[] | undefined;
  /** A picker opened: time to load the versions. */
  onOpenPicker?: () => void;
  /** Go to another comparison. */
  onCompare: (from: number, to: number) => void;
  /** Where the full list is, at the bottom of the pickers. */
  allVersions?: React.ReactNode;
  /** At the end of the bar ("Open v5"). */
  actions?: React.ReactNode;
}

/**
 * Which two versions are compared, and the way to others: a picker per side,
 * a swap, and the previous or next pair (`[` and `]`) one step at a time.
 */
export function CompareBar({
  from,
  to,
  latest,
  versions,
  onOpenPicker,
  onCompare,
  allVersions,
  actions,
}: CompareBarProps) {
  const older = Math.min(from, to) > 1 ? ([from - 1, to - 1] as const) : undefined;
  const newer = Math.max(from, to) < latest ? ([from + 1, to + 1] as const) : undefined;
  const pair = (p: readonly [number, number]) => `v${p[0]} → v${p[1]}`;
  useShortcut("[", () => older && onCompare(...older), !!older, { inGraph: true });
  useShortcut("]", () => newer && onCompare(...newer), !!newer, { inGraph: true });

  return (
    <div className="flex min-h-11 shrink-0 flex-wrap items-center gap-x-2 gap-y-1 border-b border-border px-3 py-1.5 md:px-4">
      <div className="flex items-center gap-0.5" role="group" aria-label="Compared versions">
        <Tooltip content={older ? `Older pair: ${pair(older)}` : "No older pair"} shortcut={["["]}>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Older pair"
            disabled={!older}
            onClick={() => older && onCompare(...older)}
          >
            <NavArrowLeftIcon />
          </Button>
        </Tooltip>
        <VersionPicker
          side="from"
          value={from}
          other={to}
          versions={versions}
          onOpen={onOpenPicker}
          onPick={(n) => onCompare(n, to)}
          allVersions={allVersions}
        />
        <ArrowRightIcon aria-hidden className="size-3.5 text-faint-foreground" />
        <VersionPicker
          side="to"
          value={to}
          other={from}
          versions={versions}
          onOpen={onOpenPicker}
          onPick={(n) => onCompare(from, n)}
          allVersions={allVersions}
        />
        <Tooltip content={newer ? `Newer pair: ${pair(newer)}` : "No newer pair"} shortcut={["]"]}>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Newer pair"
            disabled={!newer}
            onClick={() => newer && onCompare(...newer)}
          >
            <NavArrowRightIcon />
          </Button>
        </Tooltip>
      </div>
      <Tooltip content={`Swap sides: v${to} → v${from}`}>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Swap sides"
          disabled={from === to}
          onClick={() => onCompare(to, from)}
        >
          <DataTransferBothIcon />
        </Button>
      </Tooltip>
      {from > to && (
        <span className="text-sm text-muted-foreground">Reading backwards: newer on the left</span>
      )}
      <div className="ml-auto flex items-center gap-2">{actions}</div>
    </div>
  );
}

function VersionPicker({
  side,
  value,
  other,
  versions,
  onOpen,
  onPick,
  allVersions,
}: {
  side: "from" | "to";
  value: number;
  other: number;
  versions: VersionSummary[] | undefined;
  onOpen?: () => void;
  onPick: (number: number) => void;
  allVersions?: React.ReactNode;
}) {
  const [open, setOpen] = React.useState(false);
  return (
    <Menu
      open={open}
      onOpenChange={(next) => {
        setOpen(next);
        if (next) onOpen?.();
      }}
    >
      <MenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="font-mono text-sm font-semibold text-foreground"
        >
          v{value}
          <NavArrowDownIcon className="text-muted-foreground" />
          <span className="sr-only">
            : {side === "from" ? "compare from" : "compare to"} another version
          </span>
        </Button>
      </MenuTrigger>
      <MenuContent align="start" className="w-80">
        <MenuLabel>{side === "from" ? "Compare from" : "Compare to"}</MenuLabel>
        {versions === undefined
          ? Array.from({ length: 4 }, (_, i) => (
              <div key={i} className="mx-1 h-8 animate-pulse rounded-md bg-muted/60" />
            ))
          : versions.map((v) => (
              <MenuItem
                key={v.id}
                onSelect={() => v.number !== value && onPick(v.number)}
                className="flex items-center gap-2"
              >
                <span className="w-9 shrink-0 font-mono text-sm font-medium text-foreground tabular-nums">
                  v{v.number}
                </span>
                <span className="min-w-0 flex-1 truncate text-sm text-muted-foreground">
                  {v.message ??
                    `${originLabel[v.origin]} by ${v.author?.name ?? "a deleted account"}`}
                </span>
                {v.number === other ? (
                  <span className="shrink-0 text-xs text-faint-foreground">
                    {side === "from" ? "right side" : "left side"}
                  </span>
                ) : (
                  <span className="shrink-0 text-xs text-faint-foreground">
                    {timeAgo(v.created_at)}
                  </span>
                )}
                {v.number === value && <CheckIcon aria-label="Shown" className="text-primary" />}
              </MenuItem>
            ))}
        {allVersions && (
          <>
            <MenuSeparator />
            <MenuItem asChild>{allVersions}</MenuItem>
          </>
        )}
      </MenuContent>
    </Menu>
  );
}
