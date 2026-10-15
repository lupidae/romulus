import {
  ChatBubbleEmptyIcon,
  EditPencilIcon,
  InputFieldIcon,
  LinkIcon,
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

import { findColumn } from "../../edits/inspect";
import { targetOf } from "../../edits/plan";
import { isView, primaryKey } from "../../model";
import type { TableModel } from "../build";
import { type EditContextValue, type MenuTarget, useEdit } from "./context";

/**
 * The edit actions of a table, a column or the canvas, in one menu: opened
 * by a right click, the ⋯ of a table, or the keyboard (Shift+F10, M) on the
 * selected table. Every item names its shortcut when it has one.
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
          <MenuItem
            onSelect={() => edit.start({ kind: "newTable", position: target.at ?? edit.center() })}
          >
            <PlusIcon />
            Add a table here
            <MenuShortcut>N</MenuShortcut>
          </MenuItem>
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
