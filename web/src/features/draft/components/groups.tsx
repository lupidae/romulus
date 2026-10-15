import { Panel } from "@xyflow/react";
import * as React from "react";

import { FrameSelectIcon, TrashIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Menu,
  MenuContent,
  MenuLabel,
  MenuRadioGroup,
  MenuRadioItem,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tooltip } from "@/components/ui/tooltip";
import { type Group, GROUP_COLORS, groupColor, type Groups } from "@/features/schema/layout";
import { cn } from "@/lib/utils";

import type { DraftSession } from "../session";

const SWATCH: Record<string, string> = {
  stone: "bg-group-stone",
  red: "bg-group-red",
  amber: "bg-group-amber",
  green: "bg-group-green",
  teal: "bg-group-teal",
  blue: "bg-group-blue",
  violet: "bg-group-violet",
  pink: "bg-group-pink",
};

const colorLabel = (color: string) => color[0]!.toUpperCase() + color.slice(1);

interface GroupsMenuProps {
  session: DraftSession;
  groups: Groups;
  picked: readonly string[];
  onPickedChange: (keys: string[]) => void;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** The group whose name to focus when the menu opens (one just created). */
  focus: string | null;
}

/** The draft's groups, in the graph's toolbar: rename, recolor, delete, create. */
export function GroupsMenu({
  session,
  groups,
  picked,
  onPickedChange,
  open,
  onOpenChange,
  focus,
}: GroupsMenuProps) {
  const entries = Object.entries(groups).sort(([a], [b]) => a.localeCompare(b));
  return (
    <Popover open={open} onOpenChange={onOpenChange}>
      <Tooltip content="Groups">
        <PopoverTrigger asChild>
          <Button variant="ghost" size="icon-sm" aria-label="Groups">
            <FrameSelectIcon />
          </Button>
        </PopoverTrigger>
      </Tooltip>
      <PopoverContent align="end" className="grid w-80 gap-3 p-0">
        <div className="flex items-center justify-between px-3 pt-3">
          <h2 className="text-sm font-semibold text-foreground">Groups</h2>
          <span className="text-xs text-muted-foreground tabular">
            {entries.length === 0 ? "None yet" : entries.length}
          </span>
        </div>
        {entries.length > 0 && (
          <ul className="grid max-h-72 gap-0.5 overflow-y-auto px-1.5">
            {entries.map(([id, group]) => (
              <GroupRow key={id} id={id} group={group} session={session} focusName={focus === id} />
            ))}
          </ul>
        )}
        <div className="grid gap-2 border-t border-border px-3 py-3">
          <Button
            size="sm"
            disabled={picked.length === 0}
            onClick={() => {
              session.createGroup([...picked]);
              onPickedChange([]);
            }}
          >
            <FrameSelectIcon />
            {picked.length === 0
              ? "Group picked tables"
              : `Group ${picked.length} picked ${picked.length === 1 ? "table" : "tables"}`}
          </Button>
          <p className="text-sm text-muted-foreground">
            Shift-click tables in the graph, or press P on one, to pick them.
          </p>
        </div>
      </PopoverContent>
    </Popover>
  );
}

function GroupRow({
  id,
  group,
  session,
  focusName,
}: {
  id: string;
  group: Group;
  session: DraftSession;
  focusName: boolean;
}) {
  const [name, setName] = React.useState(group.name);
  const [editing, setEditing] = React.useState(false);
  const input = React.useRef<HTMLInputElement>(null);
  // A group just made: its name is ready to type over.
  React.useEffect(() => {
    if (!focusName) return;
    input.current?.focus();
    input.current?.select();
  }, [focusName]);
  // A rename from someone else shows, unless this person is typing one.
  if (!editing && name !== group.name) setName(group.name);
  const color = groupColor(group.color);

  const commit = () => {
    setEditing(false);
    const next = name.trim();
    if (next && next !== group.name) session.updateGroup(id, { name: next });
    else setName(group.name);
  };

  return (
    <li className="flex items-center gap-1.5 rounded-md px-1.5 py-1 hover:bg-accent/60">
      <Menu>
        <Tooltip content="Color">
          <MenuTrigger asChild>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={`Color of ${group.name}: ${colorLabel(color)}`}
            >
              <span aria-hidden className={cn("size-3 rounded-xs", SWATCH[color])} />
            </Button>
          </MenuTrigger>
        </Tooltip>
        <MenuContent align="start">
          <MenuLabel>Color</MenuLabel>
          <MenuRadioGroup
            value={color}
            onValueChange={(value) => session.updateGroup(id, { color: value })}
          >
            {GROUP_COLORS.map((c) => (
              <MenuRadioItem key={c} value={c}>
                <span aria-hidden className={cn("size-3 rounded-xs", SWATCH[c])} />
                {colorLabel(c)}
              </MenuRadioItem>
            ))}
          </MenuRadioGroup>
        </MenuContent>
      </Menu>
      <Input
        aria-label="Group name"
        ref={input}
        value={name}
        onFocus={() => setEditing(true)}
        onChange={(event) => setName(event.target.value)}
        onBlur={commit}
        onKeyDown={(event) => {
          if (event.key === "Enter") event.currentTarget.blur();
          if (event.key === "Escape") {
            event.preventDefault();
            setName(group.name);
            setEditing(false);
            event.currentTarget.blur();
          }
        }}
        maxLength={60}
        className="h-7 flex-1 border-transparent bg-transparent px-1.5 shadow-none hover:border-border focus-visible:bg-card"
      />
      <span className="w-14 shrink-0 text-right text-xs text-muted-foreground tabular">
        {group.members.length} {group.members.length === 1 ? "table" : "tables"}
      </span>
      <Tooltip content="Delete group">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={`Delete ${group.name}`}
          onClick={() => session.deleteGroup(id)}
        >
          <TrashIcon />
        </Button>
      </Tooltip>
    </li>
  );
}

/**
 * Tables picked with shift-click, floating over the graph: group them, or
 * let go. Esc does too.
 */
export function PickBar({
  picked,
  onGroup,
  onClear,
}: {
  picked: readonly string[];
  onGroup: () => void;
  onClear: () => void;
}) {
  if (picked.length === 0) return null;
  return (
    <Panel position="bottom-center" className="!mb-4">
      <div
        role="toolbar"
        aria-label="Picked tables"
        className="flex items-center gap-1.5 rounded-lg border border-border bg-popover py-1 pr-1 pl-3 text-popover-foreground shadow-popover"
      >
        <span className="text-sm font-medium tabular">
          {picked.length} {picked.length === 1 ? "table" : "tables"} picked
        </span>
        <Button size="sm" onClick={onGroup} className="ml-1.5">
          <FrameSelectIcon />
          Group
        </Button>
        <Tooltip content="Let go of the picked tables" shortcut={["Esc"]}>
          <Button size="sm" variant="ghost" onClick={onClear}>
            Clear
          </Button>
        </Tooltip>
      </div>
    </Panel>
  );
}
