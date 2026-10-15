import { ViewportPortal } from "@xyflow/react";

import { TableIcon } from "@/components/icons";

import { freshTableName, newTableRef, relationNameTaken } from "../../edits/inspect";
import { nameProblem } from "../../edits/quote";
import { NODE } from "../geometry";
import { useGraphUi } from "../graph-ui";
import { useEdit } from "./context";
import { InlineName } from "./inline-name";

/**
 * A table on its way, where it will stand: its name is asked for in place
 * (a free one is suggested), Enter creates it with an id column, Esc lets go.
 */
export function NewTable() {
  const edit = useEdit();
  const editing = useGraphUi((s) => (s.editing?.kind === "newTable" ? s.editing : null));
  if (!edit || !editing) return null;
  const { x, y } = editing.position;
  return (
    <ViewportPortal>
      <div
        data-testid="new-table"
        className="nodrag nopan absolute top-0 left-0 z-50 flex flex-col overflow-hidden rounded-lg border border-dashed border-primary bg-card shadow-popover"
        style={{ transform: `translate(${x}px, ${y}px)`, width: NODE.minWidth }}
      >
        <div
          className="flex shrink-0 items-center gap-2 border-b border-border px-3"
          style={{ height: NODE.header }}
        >
          <TableIcon aria-hidden className="size-4 shrink-0 text-muted-foreground" />
          <InlineName
            aria-label="New table name"
            initial={freshTableName(edit.schema, "public")}
            acceptInitial
            validate={(typed) => {
              const ref = newTableRef(edit.schema, typed);
              return nameProblem(ref.name) ?? relationNameTaken(edit.schema, ref.schema, ref.name);
            }}
            onCommit={(typed) =>
              edit.emit(
                { kind: "addTable", table: newTableRef(edit.schema, typed) },
                { position: editing.position },
              )
            }
            onCancel={edit.stop}
            className="font-mono text-sm font-semibold sm:text-sm"
          />
        </div>
        <div
          className="flex items-center gap-1.5 px-3 font-mono text-xs"
          style={{ height: NODE.row, marginTop: NODE.padding }}
        >
          <span className="w-3.5" />
          <span className="text-foreground">id</span>
          <span className="ml-auto text-faint-foreground">bigint</span>
        </div>
        <p className="px-3 pb-2 text-xs text-muted-foreground" style={{ marginTop: NODE.padding }}>
          Enter to create, Esc to cancel
        </p>
      </div>
    </ViewportPortal>
  );
}
