import { Panel, ViewportPortal } from "@xyflow/react";
import * as React from "react";

import { FrameSelectIcon, TrashIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  Menu,
  MenuContent,
  MenuLabel,
  MenuRadioGroup,
  MenuRadioItem,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tooltip } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

import { type Group, GROUP_COLORS, type GroupColor, groupColor } from "../../layout";
import { useGraphUi } from "../graph-ui";
import { type EditContextValue, useEdit } from "./context";
import { colorLabel, GROUP_NAME_MAX, groupNameProblem, SWATCH, tablesLabel } from "./group-labels";

// The group editing parts that are not the frames themselves (group-node.tsx
// draws those): the toolbar's list of groups, the bar over picked tables,
// the colors as menu items, and what a table dragged over a group will do.

/** The palette as radio items, for a menu (a group's own, the panel's swatch). */
export function GroupColorItems({
  value,
  onChange,
}: {
  value: string;
  onChange: (color: GroupColor) => void;
}) {
  return (
    <MenuRadioGroup value={groupColor(value)} onValueChange={(v) => onChange(groupColor(v))}>
      {GROUP_COLORS.map((c) => (
        <MenuRadioItem key={c} value={c}>
          <span aria-hidden className={cn("size-3 rounded-xs", SWATCH[c])} />
          {colorLabel(c)}
        </MenuRadioItem>
      ))}
    </MenuRadioGroup>
  );
}

/**
 * The draft's groups in the graph's toolbar: every group with its color,
 * name and size, renamed, recolored or deleted from the keyboard; and the
 * picked tables grouped.
 */
export function GroupsPanel() {
  const edit = useEdit();
  const groups = useGraphUi((s) => s.groups);
  const picked = useGraphUi((s) => s.picked);
  const [open, setOpen] = React.useState(false);
  if (!edit?.editor.groups) return null;
  const entries = Object.entries(groups).sort(([, a], [, b]) => a.name.localeCompare(b.name));
  return (
    <Popover open={open} onOpenChange={setOpen}>
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
              <GroupRow key={id} id={id} group={group} edit={edit} />
            ))}
          </ul>
        )}
        <div className="grid gap-2 border-t border-border px-3 py-3">
          <Button
            size="sm"
            disabled={picked.length === 0}
            onClick={() => {
              setOpen(false);
              edit.makeGroup([...picked]);
            }}
          >
            <FrameSelectIcon />
            {picked.length === 0 ? "Group picked tables" : `Group ${tablesLabel(picked.length)}`}
          </Button>
          <p className="text-sm text-muted-foreground">
            Shift-click tables, or press <Kbd>P</Kbd> on one, to pick them. <Kbd>G</Kbd> groups
            them; drag a table onto a group to add it.
          </p>
        </div>
      </PopoverContent>
    </Popover>
  );
}

function GroupRow({ id, group, edit }: { id: string; group: Group; edit: EditContextValue }) {
  const [name, setName] = React.useState(group.name);
  const [editing, setEditing] = React.useState(false);
  // A rename from someone else shows, unless this person is typing one.
  if (!editing && name !== group.name) setName(group.name);
  const color = groupColor(group.color);
  const groups = edit.editor.groups!;

  const commit = () => {
    setEditing(false);
    const next = name.trim();
    if (next && next !== group.name && !groupNameProblem(next)) groups.update(id, { name: next });
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
          <GroupColorItems value={color} onChange={(c) => groups.update(id, { color: c })} />
        </MenuContent>
      </Menu>
      <Input
        aria-label={`Name of ${group.name}`}
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
        maxLength={GROUP_NAME_MAX}
        className="h-7 flex-1 border-transparent bg-transparent px-1.5 shadow-none hover:border-border focus-visible:bg-card"
      />
      <span className="w-14 shrink-0 text-right text-xs text-muted-foreground tabular">
        {tablesLabel(group.members.length)}
      </span>
      <Tooltip content="Delete group (its tables stay)">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={`Delete ${group.name}`}
          onClick={() => groups.remove(id)}
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
export function PickBar() {
  const edit = useEdit();
  const picked = useGraphUi((s) => s.picked);
  if (!edit?.editor.groups || picked.length === 0) return null;
  return (
    <Panel position="bottom-center" className="!mb-4">
      <div
        role="toolbar"
        aria-label="Picked tables"
        className="flex items-center gap-1.5 rounded-lg border border-border bg-popover py-1 pr-1 pl-3 text-popover-foreground shadow-popover"
      >
        <span className="text-sm font-medium tabular">{tablesLabel(picked.length)} picked</span>
        <Tooltip content="Group the picked tables" shortcut={["G"]}>
          <Button size="sm" onClick={() => edit.makeGroup([...picked])} className="ml-1.5">
            <FrameSelectIcon />
            Group
          </Button>
        </Tooltip>
        <Tooltip content="Let go of the picked tables" shortcut={["Esc"]}>
          <Button size="sm" variant="ghost" onClick={() => edit.pick([])}>
            Clear
          </Button>
        </Tooltip>
      </div>
    </Panel>
  );
}

/**
 * What dropping the dragged table will do to its group, above it while it
 * moves ("Add to Billing", "Remove from Billing"): the drop is never a
 * surprise. Screen size at any zoom.
 */
export function DropHint({
  at,
  text,
  color,
}: {
  at: { x: number; y: number };
  text: string;
  color: GroupColor | null;
}) {
  return (
    <ViewportPortal>
      <div
        className="pointer-events-none absolute top-0 left-0 z-50"
        style={{ transform: `translate(${at.x}px, ${at.y}px)` }}
      >
        <div
          role="status"
          className={cn(
            "absolute bottom-1.5 left-0 flex origin-bottom-left items-center gap-1.5 rounded-md border border-border bg-popover px-2 py-1 text-xs font-medium whitespace-nowrap text-popover-foreground shadow-popover",
            "[transform:scale(calc(1/var(--graph-zoom,1)))]",
          )}
        >
          <span
            aria-hidden
            className={cn("size-2 rounded-xs", color ? SWATCH[color] : "bg-border-strong")}
          />
          {text}
        </div>
      </div>
    </ViewportPortal>
  );
}
