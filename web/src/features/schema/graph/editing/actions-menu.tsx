import {
  ChatBubbleEmptyIcon,
  EditPencilIcon,
  FrameSelectIcon,
  InputFieldIcon,
  LinkIcon,
  MinusCircleIcon,
  PlusIcon,
  TableIcon,
  TextSquareIcon,
  TrashIcon,
} from "@/components/icons";
import {
  Menu,
  MenuCheckboxItem,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuSeparator,
  MenuShortcut,
  MenuSub,
  MenuSubContent,
  MenuSubTrigger,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { toast } from "@/components/ui/toaster";
import { cn } from "@/lib/utils";

import { findColumn } from "../../edits/inspect";
import { targetOf } from "../../edits/plan";
import { groupColor } from "../../layout";
import { isView, primaryKey } from "../../model";
import type { TableModel } from "../build";
import { useGraphUiStore } from "../graph-ui";
import { type EditContextValue, type MenuTarget, useEdit } from "./context";
import { SWATCH, tablesLabel } from "./group-labels";
import { GroupColorItems } from "./groups";

/**
 * The edit actions of a table, a column, a group or the canvas, in one
 * menu: opened by a right click, the ⋯ of a table or a group, or the
 * keyboard (Shift+F10, M) on the selected table. Every item names its
 * shortcut when it has one.
 */
export function ActionsMenu({
  target,
  onClose,
}: {
  target: MenuTarget | null;
  onClose: () => void;
}) {
  const edit = useEdit();
  if (!edit || !target) return null;
  const table = target.table ? edit.graph.byKey.get(target.table) : undefined;
  return (
    <Menu open onOpenChange={(open) => !open && onClose()} modal={false}>
      <MenuTrigger asChild>
        {/* The menu's anchor: where it was asked for. */}
        <span
          aria-hidden
          className="pointer-events-none fixed size-0"
          style={{ left: target.x, top: target.y }}
        />
      </MenuTrigger>
      <MenuContent
        align="start"
        sideOffset={2}
        className="min-w-56"
        // Keys go back to the graph, or to the editor the item opened.
        onCloseAutoFocus={(event) => event.preventDefault()}
      >
        {table && target.column ? (
          <ColumnItems edit={edit} table={table} column={target.column} label />
        ) : table ? (
          <TableItems edit={edit} table={table} />
        ) : (
          <>
            {target.group && <GroupItems edit={edit} group={target.group} />}
            <MenuItem
              onSelect={() =>
                edit.start({ kind: "newTable", position: target.at ?? edit.center() })
              }
            >
              <PlusIcon />
              Add a table here
              <MenuShortcut>N</MenuShortcut>
            </MenuItem>
          </>
        )}
      </MenuContent>
    </Menu>
  );
}

function TableItems({ edit, table }: { edit: EditContextValue; table: TableModel }) {
  const entity = table.entity;
  const view = isView(entity.kind);
  const word = view ? (entity.kind === "m" ? "materialized view" : "view") : "table";
  return (
    <>
      <MenuLabel className="truncate font-mono">{table.name}</MenuLabel>
      <MenuItem onSelect={() => edit.start({ kind: "renameTable", table: table.key })}>
        <EditPencilIcon />
        Rename {word}
        <MenuShortcut>F2</MenuShortcut>
      </MenuItem>
      {!view && (
        <MenuItem onSelect={() => edit.start({ kind: "addColumn", table: table.key })}>
          <PlusIcon />
          Add column
          <MenuShortcut>A</MenuShortcut>
        </MenuItem>
      )}
      <MenuItem onSelect={() => edit.start({ kind: "comment", table: table.key })}>
        <ChatBubbleEmptyIcon />
        {entity.comment ? "Edit comment" : "Add comment"}
      </MenuItem>
      {table.columns.length > 0 && (
        <MenuSub>
          <MenuSubTrigger>
            <TableIcon />
            Columns
          </MenuSubTrigger>
          <MenuSubContent className="max-h-80 overflow-y-auto">
            {table.columns.map((c) => (
              <MenuSub key={c.name}>
                <MenuSubTrigger className="font-mono text-xs">{c.name}</MenuSubTrigger>
                <MenuSubContent className="min-w-52">
                  <ColumnItems edit={edit} table={table} column={c.name} />
                </MenuSubContent>
              </MenuSub>
            ))}
          </MenuSubContent>
        </MenuSub>
      )}
      <TableGroupItems edit={edit} table={table} />
      <MenuSeparator />
      <MenuItem
        variant="destructive"
        onSelect={() => edit.start({ kind: "drop", table: table.key })}
      >
        <TrashIcon />
        Drop {word}…<MenuShortcut>⌫</MenuShortcut>
      </MenuItem>
    </>
  );
}

function ColumnItems({
  edit,
  table,
  column,
  label = false,
}: {
  edit: EditContextValue;
  table: TableModel;
  column: string;
  label?: boolean;
}) {
  const entity = table.entity;
  const c = findColumn(entity, column);
  if (!c) return null;
  const view = isView(entity.kind);
  const inKey = primaryKey(entity)?.columns?.includes(column) ?? false;
  const at = { table: table.key, column };
  return (
    <>
      {label && (
        <MenuLabel className="truncate font-mono">
          {table.name}.{column}
        </MenuLabel>
      )}
      <MenuItem onSelect={() => edit.start({ kind: "renameColumn", ...at })}>
        <EditPencilIcon />
        Rename column
      </MenuItem>
      {!view && (
        <>
          <MenuItem onSelect={() => edit.start({ kind: "type", ...at })}>
            <TextSquareIcon />
            Change type…
          </MenuItem>
          <MenuItem
            onSelect={() => edit.start({ kind: "default", ...at })}
            disabled={c.identity !== null || c.generated !== null}
          >
            <InputFieldIcon />
            {c.default === null ? "Set default…" : "Edit default…"}
          </MenuItem>
          <MenuCheckboxItem
            checked={c.not_null === true}
            disabled={inKey || c.identity !== null}
            onCheckedChange={(checked) =>
              edit.emit({
                kind: "setNotNull",
                table: targetOf(entity),
                column,
                notNull: checked === true,
              })
            }
          >
            Not null
          </MenuCheckboxItem>
        </>
      )}
      <MenuItem onSelect={() => edit.start({ kind: "comment", ...at })}>
        <ChatBubbleEmptyIcon />
        {c.comment ? "Edit comment" : "Add comment"}
      </MenuItem>
      {!view && (
        <MenuItem onSelect={() => edit.start({ kind: "foreignKey", ...at })}>
          <LinkIcon />
          Add foreign key…
        </MenuItem>
      )}
      {!view && (
        <>
          <MenuSeparator />
          <MenuItem variant="destructive" onSelect={() => edit.start({ kind: "drop", ...at })}>
            <TrashIcon />
            Drop column…
          </MenuItem>
        </>
      )}
    </>
  );
}

/**
 * A table's group: make one (of it, or of the picked tables it is among),
 * move it to another, take it out. Read when the menu opens.
 */
function TableGroupItems({ edit, table }: { edit: EditContextValue; table: TableModel }) {
  const store = useGraphUiStore();
  const groupEditor = edit.editor.groups;
  if (!groupEditor) return null;
  const { groups, groupOf, picked } = store.get();
  const current = groupOf.get(table.key) ?? null;
  const others = Object.entries(groups)
    .filter(([id]) => id !== current)
    .sort(([, a], [, b]) => a.name.localeCompare(b.name));
  const withPicked = picked.includes(table.key) && picked.length > 1;
  return (
    <>
      <MenuSeparator />
      <MenuItem onSelect={() => edit.makeGroup(withPicked ? [...picked] : [table.key])}>
        <FrameSelectIcon />
        {withPicked ? `Group the ${picked.length} picked tables` : "New group"}
        <MenuShortcut>G</MenuShortcut>
      </MenuItem>
      {others.length > 0 && (
        <MenuSub>
          <MenuSubTrigger>
            <FrameSelectIcon />
            {current ? "Move to group" : "Add to group"}
          </MenuSubTrigger>
          <MenuSubContent className="max-h-80 min-w-44 overflow-y-auto">
            {others.map(([id, g]) => (
              <MenuItem key={id} onSelect={() => groupEditor.assign([table.key], id)}>
                <span
                  aria-hidden
                  className={cn("size-3 rounded-xs", SWATCH[groupColor(g.color)])}
                />
                <span className="truncate">{g.name}</span>
              </MenuItem>
            ))}
          </MenuSubContent>
        </MenuSub>
      )}
      {current && groups[current] && (
        <MenuItem onSelect={() => groupEditor.assign([table.key], null)}>
          <MinusCircleIcon />
          <span className="truncate">Remove from {groups[current].name}</span>
        </MenuItem>
      )}
    </>
  );
}

/** A group's own actions: on its label's ⋯, or a right click anywhere in it. */
function GroupItems({ edit, group }: { edit: EditContextValue; group: string }) {
  const store = useGraphUiStore();
  const groupEditor = edit.editor.groups;
  const g = store.get().groups[group];
  if (!groupEditor || !g) return null;
  return (
    <>
      <MenuLabel className="truncate">{g.name}</MenuLabel>
      <MenuItem onSelect={() => edit.start({ kind: "renameGroup", group })}>
        <EditPencilIcon />
        Rename group
      </MenuItem>
      <MenuSub>
        <MenuSubTrigger>
          <span aria-hidden className={cn("size-3 rounded-xs", SWATCH[groupColor(g.color)])} />
          Color
        </MenuSubTrigger>
        <MenuSubContent className="min-w-36">
          <GroupColorItems
            value={g.color}
            onChange={(color) => groupEditor.update(group, { color })}
          />
        </MenuSubContent>
      </MenuSub>
      <MenuItem onSelect={() => edit.pick([...g.members])}>
        <TableIcon />
        Pick its tables
      </MenuItem>
      <MenuItem
        variant="destructive"
        onSelect={() => {
          groupEditor.remove(group);
          toast(`Deleted ${g.name}`, {
            description: `Its ${tablesLabel(g.members.length)} stay where they are. Undo brings the group back.`,
          });
        }}
      >
        <TrashIcon />
        Delete group
      </MenuItem>
      <MenuSeparator />
    </>
  );
}
