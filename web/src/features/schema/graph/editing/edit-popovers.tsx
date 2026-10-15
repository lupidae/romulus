import { useId, useMemo, useState } from "react";

import { ArrowLeftIcon, LinkIcon, WarningTriangleIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Combobox, type ComboboxOption } from "@/components/ui/combobox";
import { Field } from "@/components/ui/field";
import { Input, Textarea } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Label } from "@/components/ui/label";
import { Popover, PopoverAnchor, PopoverContent } from "@/components/ui/popover";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { modKey } from "@/lib/platform";
import { cn } from "@/lib/utils";

import {
  type Dependent,
  findColumn,
  columnNameTaken,
  referenceableColumns,
} from "../../edits/inspect";
import { dropColumn, dropTable, foreignKey, setType, targetOf } from "../../edits/plan";
import { nameProblem } from "../../edits/quote";
import { baseType, comparable, typeFromText } from "../../edits/types";
import {
  type Column,
  type Entity,
  isView,
  qualifiedName,
  type ReferentialActionCode,
} from "../../model";
import { useGraphUi } from "../graph-ui";
import { type EditContextValue, useEdit, virtualAnchor } from "./context";
import { TypePicker } from "./type-picker";
import type { Editing } from "./types";

type PopoverEditing = Exclude<
  Editing,
  { kind: "renameTable" } | { kind: "renameColumn" } | { kind: "newTable" }
>;

const isPopover = (e: Editing | null): e is PopoverEditing =>
  !!e && e.kind !== "renameTable" && e.kind !== "renameColumn" && e.kind !== "newTable";

/**
 * The small forms an edit asks for, each in a popover beside the table or
 * column it is about: one at a time, never modal, Esc to leave.
 */
export function EditPopovers() {
  const edit = useEdit();
  const editing = useGraphUi((s) => s.editing);
  if (!edit || !isPopover(editing)) return null;
  const table = edit.graph.byKey.get(editing.table);
  if (!table) return null;
  const column = "column" in editing ? editing.column : undefined;
  return (
    <EditPopover
      key={`${editing.kind}:${editing.table}:${column ?? ""}`}
      edit={edit}
      editing={editing}
      entity={table.entity}
      column={column}
    />
  );
}

const WIDTH: Partial<Record<Editing["kind"], string>> = {
  addColumn: "w-80",
  foreignKey: "w-80",
  comment: "w-80",
  drop: "w-80",
};

function EditPopover({
  edit,
  editing,
  entity,
  column,
}: {
  edit: EditContextValue;
  editing: PopoverEditing;
  entity: Entity;
  column?: string;
}) {
  // The row for column forms, the whole table otherwise; adding a column
  // points at where it will go, under the last one.
  const anchor = useMemo(
    () => virtualAnchor(() => edit.element(editing.table, column)),
    [edit, editing.table, column],
  );
  return (
    <Popover open onOpenChange={(open) => !open && edit.stop()}>
      <PopoverAnchor virtualRef={anchor} />
      <PopoverContent
        side="right"
        align="start"
        sideOffset={10}
        collisionPadding={12}
        updatePositionStrategy="always"
        // An action that is the obvious next step takes the focus (`data-autofocus`);
        // otherwise the first field does.
        onOpenAutoFocus={(event) => {
          const first = (event.currentTarget as HTMLElement | null)?.querySelector<HTMLElement>(
            "[data-autofocus]",
          );
          if (first) {
            event.preventDefault();
            first.focus();
          }
        }}
        onCloseAutoFocus={(event) => event.preventDefault()}
        className={cn("grid gap-3", WIDTH[editing.kind] ?? "w-72")}
        aria-label={TITLES[editing.kind]}
      >
        {editing.kind === "addColumn" && <AddColumnForm edit={edit} entity={entity} />}
        {editing.kind === "type" && column && (
          <TypeForm edit={edit} entity={entity} column={column} />
        )}
        {editing.kind === "default" && column && (
          <DefaultForm edit={edit} entity={entity} column={column} />
        )}
        {editing.kind === "comment" && <CommentForm edit={edit} entity={entity} column={column} />}
        {editing.kind === "drop" && <DropForm edit={edit} entity={entity} column={column} />}
        {editing.kind === "foreignKey" && column && (
          <ForeignKeyForm edit={edit} entity={entity} column={column} to={editing.to} />
        )}
        {editing.kind === "failure" && (
          <FailureNote edit={edit} table={editing.table} column={column} />
        )}
      </PopoverContent>
    </Popover>
  );
}

const TITLES: Record<Editing["kind"], string> = {
  renameTable: "Rename table",
  renameColumn: "Rename column",
  addColumn: "Add a column",
  type: "Change the type",
  default: "Default value",
  comment: "Comment",
  drop: "Drop",
  foreignKey: "Add a foreign key",
  newTable: "New table",
  failure: "This edit failed",
};

function Header({ title, detail }: { title: string; detail?: React.ReactNode }) {
  return (
    <div className="grid gap-0.5">
      <h2 className="text-sm font-semibold text-foreground">{title}</h2>
      {detail && <p className="text-sm text-muted-foreground">{detail}</p>}
    </div>
  );
}

function Name({ children }: { children: React.ReactNode }) {
  return <span className="font-mono text-xs text-foreground">{children}</span>;
}

function Footer({ children }: { children: React.ReactNode }) {
  return <div className="flex items-center justify-end gap-2 pt-1">{children}</div>;
}

/** Why a default expression cannot be written as is (it would end the statement), or null. */
function expressionProblem(text: string): string | null {
  let quote: string | null = null;
  let depth = 0;
  for (let i = 0; i < text.length; i++) {
    const ch = text[i]!;
    if (quote) {
      if (ch === quote) quote = text[i + 1] === quote ? (i++, quote) : null;
      continue;
    }
    if (ch === "'" || ch === '"') quote = ch;
    else if (ch === "(") depth++;
    else if (ch === ")" && --depth < 0) return "A parenthesis closes that never opened.";
    else if (ch === ";") return "A default is one expression: no semicolons.";
    else if ((ch === "-" && text[i + 1] === "-") || (ch === "/" && text[i + 1] === "*"))
      return "Leave comments out of the default.";
  }
  if (quote) return "A quote is left open.";
  if (depth > 0) return "A parenthesis is left open.";
  return null;
}

// ---- Add a column -------------------------------------------------------------

function AddColumnForm({ edit, entity }: { edit: EditContextValue; entity: Entity }) {
  const id = useId();
  const [name, setName] = useState("");
  const [type, setType] = useState("text");
  const [notNull, setNotNull] = useState(false);
  const [initial, setInitial] = useState("");
  const [tried, setTried] = useState(false);

  const trimmed = name.trim();
  const nameError = nameProblem(trimmed) ?? columnNameTaken(entity, trimmed);
  const choice = typeFromText(edit.schema, type);
  const typeError = choice ? null : "Pick a type, or type one such as varchar(80).";
  const defaultError = initial.trim() ? expressionProblem(initial) : null;

  const submit = () => {
    setTried(true);
    if (nameError || !choice || defaultError) return;
    edit.emit({
      kind: "addColumn",
      table: targetOf(entity),
      column: { name: trimmed, type: choice, notNull, default: initial.trim() || null },
    });
  };

  return (
    <form
      className="grid gap-3"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <Header title={`Add a column to ${entity.name}`} />
      <Field label="Name" error={tried ? nameError : undefined}>
        <Input
          value={name}
          onChange={(event) => setName(event.target.value)}
          placeholder="e.g. created_at"
          spellCheck={false}
          autoComplete="off"
          className="font-mono"
        />
      </Field>
      <Field label="Type" error={tried ? typeError : undefined}>
        <TypePicker schema={edit.schema} value={type} onValueChange={setType} />
      </Field>
      <Field
        label="Default"
        description="An expression: now(), 0, 'draft'. Leave empty for none."
        error={defaultError ?? undefined}
      >
        <Input
          value={initial}
          onChange={(event) => setInitial(event.target.value)}
          placeholder="None"
          spellCheck={false}
          autoComplete="off"
          className="font-mono"
        />
      </Field>
      <div className="flex items-start gap-2">
        <Checkbox
          id={`${id}-not-null`}
          checked={notNull}
          onCheckedChange={(checked) => setNotNull(checked === true)}
          aria-describedby={notNull && !initial.trim() ? `${id}-not-null-hint` : undefined}
          className="mt-0.5"
        />
        <div className="grid gap-0.5">
          <Label htmlFor={`${id}-not-null`}>Not null</Label>
          {notNull && !initial.trim() && (
            <p id={`${id}-not-null-hint`} className="text-sm text-muted-foreground">
              On a table with rows, this needs a default.
            </p>
          )}
        </div>
      </div>
      <Footer>
        <Button type="button" variant="ghost" size="sm" onClick={edit.stop}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" size="sm">
          Add column
        </Button>
      </Footer>
    </form>
  );
}

// ---- Change a type --------------------------------------------------------------

function TypeForm({
  edit,
  entity,
  column,
}: {
  edit: EditContextValue;
  entity: Entity;
  column: string;
}) {
  const current = findColumn(entity, column);
  const [type, setTypeText] = useState(current?.type ?? "");
  const choice = typeFromText(edit.schema, type);
  const planned = choice ? setType(edit.schema, targetOf(entity), column, choice) : null;
  const unchanged = !!choice && !planned;

  const submit = (text = type) => {
    const picked = typeFromText(edit.schema, text);
    if (!picked) return;
    const plan = setType(edit.schema, targetOf(entity), column, picked);
    if (plan) edit.emit(plan);
    else edit.stop();
  };

  return (
    <form
      className="grid gap-3"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <Header
        title="Change the type"
        detail={
          <>
            <Name>
              {entity.name}.{column}
            </Name>{" "}
            is <Name>{current?.type}</Name>
          </>
        }
      />
      <Field
        label="New type"
        description={
          planned?.kind === "setType" && planned.using
            ? `Values are converted with USING ${column}::${planned.type.sql}.`
            : unchanged
              ? "That is its type already."
              : undefined
        }
      >
        <TypePicker schema={edit.schema} value={type} onValueChange={setTypeText} />
      </Field>
      <Footer>
        <Button type="button" variant="ghost" size="sm" onClick={edit.stop}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" size="sm" disabled={!choice || unchanged}>
          Change type
        </Button>
      </Footer>
    </form>
  );
}

// ---- Default ----------------------------------------------------------------------

/** Defaults people reach for, by the column's type. */
function suggestions(column: Column, edit: EditContextValue): string[] {
  const base = baseType(column, edit.schema);
  if (base.startsWith("timestamp") || base === "date") return ["now()", "current_date"];
  if (base === "uuid") return ["gen_random_uuid()"];
  if (base === "boolean") return ["false", "true"];
  if (["integer", "bigint", "smallint", "numeric", "real", "double precision"].includes(base))
    return ["0"];
  if (base === "jsonb" || base === "json") return [`'{}'::${base}`, `'[]'::${base}`];
  if (base === "text" || base.startsWith("character")) return ["''"];
  return [];
}

function DefaultForm({
  edit,
  entity,
  column,
}: {
  edit: EditContextValue;
  entity: Entity;
  column: string;
}) {
  const current = findColumn(entity, column);
  const [value, setValue] = useState(current?.default ?? "");
  if (!current) return null;
  const fixed = current.identity !== null || current.generated !== null;
  const problem = value.trim() ? expressionProblem(value) : null;
  const target = targetOf(entity);
  const set = (expression: string | null) => {
    if (expression === (current.default ?? null)) return edit.stop();
    edit.emit({ kind: "setDefault", table: target, column, default: expression });
  };

  if (fixed) {
    return (
      <>
        <Header
          title="Default value"
          detail={
            current.identity
              ? "An identity column takes its values from its sequence."
              : "A generated column computes its values."
          }
        />
        <Footer>
          <Button size="sm" onClick={edit.stop}>
            OK
          </Button>
        </Footer>
      </>
    );
  }
  const options = suggestions(current, edit);
  return (
    <form
      className="grid gap-3"
      onSubmit={(event) => {
        event.preventDefault();
        if (!problem) set(value.trim() || null);
      }}
    >
      <Header
        title="Default value"
        detail={
          <>
            For{" "}
            <Name>
              {entity.name}.{column}
            </Name>
            , when a row leaves it out
          </>
        }
      />
      <Field label="Expression" error={problem ?? undefined}>
        <Input
          value={value}
          onChange={(event) => setValue(event.target.value)}
          placeholder="None"
          spellCheck={false}
          autoComplete="off"
          className="font-mono"
        />
      </Field>
      {options.length > 0 && (
        <div className="flex flex-wrap gap-1.5" aria-label="Suggestions">
          {options.map((option) => (
            <Button
              key={option}
              type="button"
              size="sm"
              variant="secondary"
              className="font-mono text-xs"
              onClick={() => setValue(option)}
            >
              {option}
            </Button>
          ))}
        </div>
      )}
      <Footer>
        {current.default !== null && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="mr-auto"
            onClick={() => set(null)}
          >
            Remove default
          </Button>
        )}
        <Button type="button" variant="ghost" size="sm" onClick={edit.stop}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" size="sm" disabled={!!problem}>
          Set default
        </Button>
      </Footer>
    </form>
  );
}

// ---- Comment ----------------------------------------------------------------------

function CommentForm({
  edit,
  entity,
  column,
}: {
  edit: EditContextValue;
  entity: Entity;
  column?: string;
}) {
  const current =
    column === undefined ? entity.comment : (findColumn(entity, column)?.comment ?? null);
  const [value, setValue] = useState(current ?? "");
  const save = (comment: string | null) => {
    if (comment === current) return edit.stop();
    edit.emit({ kind: "setComment", table: targetOf(entity), column, comment });
  };
  const submit = () => save(value.trim() ? value.trim() : null);
  return (
    <form
      className="grid gap-3"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
    >
      <Header
        title={current ? "Edit the comment" : "Add a comment"}
        detail={
          <>
            On <Name>{column === undefined ? entity.name : `${entity.name}.${column}`}</Name>, kept
            in the schema as COMMENT ON
          </>
        }
      />
      <Textarea
        aria-label="Comment"
        value={value}
        onChange={(event) => setValue(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
            event.preventDefault();
            submit();
          }
        }}
        placeholder="What it holds, in a sentence"
        className="min-h-16"
      />
      <Footer>
        {current && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="mr-auto"
            onClick={() => save(null)}
          >
            Remove
          </Button>
        )}
        <Button type="button" variant="ghost" size="sm" onClick={edit.stop}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" size="sm">
          Save
          <Kbd className="ml-0.5 border-primary-foreground/25 bg-primary-foreground/10 text-primary-foreground shadow-none">
            {modKey}↵
          </Kbd>
        </Button>
      </Footer>
    </form>
  );
}

// ---- Drop ---------------------------------------------------------------------------

const MAX_LISTED = 6;

function DropForm({
  edit,
  entity,
  column,
}: {
  edit: EditContextValue;
  entity: Entity;
  column?: string;
}) {
  const target = targetOf(entity);
  const { edit: drop, dependents } =
    column === undefined ? dropTable(edit.schema, target) : dropColumn(edit.schema, target, column);
  const what =
    column !== undefined
      ? "column"
      : isView(entity.kind)
        ? entity.kind === "m"
          ? "materialized view"
          : "view"
        : "table";
  return (
    <div className="grid gap-3">
      <Header
        title={`Drop ${what} ${column === undefined ? entity.name : `${entity.name}.${column}`}?`}
        detail={
          dependents.all.length === 0
            ? "Nothing else depends on it."
            : dependents.cascade
              ? "These go with it (CASCADE):"
              : "These go with it:"
        }
      />
      {dependents.all.length > 0 && <DependentList dependents={dependents.all} />}
      <p className="text-sm text-muted-foreground">
        Undo with <Kbd>{modKey}</Kbd> <Kbd>Z</Kbd>.
      </p>
      <Footer>
        <Button variant="ghost" size="sm" onClick={edit.stop}>
          Cancel
        </Button>
        <Button
          // Focused first: Enter confirms, Esc cancels.
          data-autofocus=""
          variant="destructive"
          size="sm"
          onClick={() => edit.emit(drop)}
        >
          Drop {what}
        </Button>
      </Footer>
    </div>
  );
}

function DependentList({ dependents }: { dependents: Dependent[] }) {
  const shown = dependents.slice(0, MAX_LISTED);
  return (
    <ul className="grid gap-1 rounded-md border border-border bg-muted/50 px-2.5 py-2">
      {shown.map((d) => (
        <li
          key={`${d.kind}:${d.table.schema}.${d.table.name}:${d.name}`}
          className="flex min-w-0 items-baseline gap-2 text-sm"
        >
          <span className="min-w-0 truncate font-mono text-xs text-foreground">{d.name}</span>
          <span className="ml-auto shrink-0 text-xs text-muted-foreground">
            {d.kind === "view" || d.kind === "materialized view"
              ? d.kind
              : `${d.kind} on ${d.table.name}`}
          </span>
        </li>
      ))}
      {dependents.length > MAX_LISTED && (
        <li className="text-xs text-muted-foreground">and {dependents.length - MAX_LISTED} more</li>
      )}
    </ul>
  );
}

// ---- Foreign key ----------------------------------------------------------------------

const ON_DELETE: { value: ReferentialActionCode; label: string; description: string }[] = [
  { value: "a", label: "No action", description: "Refuse to delete a referenced row" },
  { value: "c", label: "Cascade", description: "Delete the rows that reference it" },
  { value: "n", label: "Set null", description: "Empty the reference" },
  { value: "r", label: "Restrict", description: "Refuse at once, even if deferred" },
];

function ForeignKeyForm({
  edit,
  entity,
  column,
  to,
}: {
  edit: EditContextValue;
  entity: Entity;
  column: string;
  to?: { table: string; column: string };
}) {
  const source = findColumn(entity, column);
  const [target, setTarget] = useState(to);
  const [query, setQuery] = useState("");
  const [onDelete, setOnDelete] = useState<ReferentialActionCode>("a");

  // Columns it may point at: primary keys and unique columns of tables, the
  // comparable ones first.
  const candidates = useMemo(() => {
    if (!source) return [];
    const out: (ComboboxOption & { ok: boolean })[] = [];
    for (const t of edit.graph.tables) {
      if (t.kind !== "table" && t.kind !== "partitioned") continue;
      for (const c of referenceableColumns(t.entity)) {
        const ok = comparable(baseType(source, edit.schema), baseType(c, edit.schema));
        out.push({
          value: `${t.key}\u0000${c.name}`,
          label: `${t.name}.${c.name}`,
          description: c.type,
          group: ok ? "Matching type" : "Other types",
          ok,
        });
      }
    }
    return out.sort((a, b) => Number(b.ok) - Number(a.ok));
  }, [edit, source]);
  const filtered = candidates.filter((c) =>
    c.label.toLowerCase().includes(query.trim().toLowerCase()),
  );

  const targetTable = target ? edit.graph.byKey.get(target.table) : undefined;
  const plan =
    target && targetTable
      ? foreignKey(
          edit.schema,
          targetOf(entity),
          column,
          targetTable.entity,
          target.column,
          onDelete,
        )
      : null;
  const nullable = !source?.not_null;

  if (!source) return null;
  return (
    <form
      className="grid gap-3"
      onSubmit={(event) => {
        event.preventDefault();
        if (plan?.ok) edit.emit(plan.edit);
      }}
    >
      <Header
        title="Add a foreign key"
        detail={
          <span className="flex flex-wrap items-center gap-1.5">
            <Name>
              {entity.name}.{column}
            </Name>
            <LinkIcon aria-hidden className="size-3.5 text-info" />
            {target && targetTable ? (
              <Name>
                {targetTable.name}.{target.column}
              </Name>
            ) : (
              <span>a primary key or unique column</span>
            )}
          </span>
        }
      />
      {!to && (
        <Field label="References">
          <Combobox
            value={query}
            onValueChange={(value) => {
              setQuery(value);
              setTarget(undefined);
            }}
            options={filtered}
            onPick={(option) => {
              const [table, col] = option.value.split("\u0000") as [string, string];
              setTarget({ table, column: col });
              setQuery(option.label);
            }}
            monospace
            placeholder="Find a table"
            empty="No primary key or unique column matches."
          />
        </Field>
      )}
      {plan && !plan.ok && (
        <p role="alert" className="flex gap-2 text-sm text-destructive">
          <WarningTriangleIcon aria-hidden className="mt-0.5 size-4 shrink-0" />
          {plan.reason}
        </p>
      )}
      <Field label="On delete">
        <Select value={onDelete} onValueChange={(v) => setOnDelete(v as ReferentialActionCode)}>
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {ON_DELETE.map((option) => (
              <SelectItem
                key={option.value}
                value={option.value}
                description={
                  option.value === "n" && !nullable ? `${column} is NOT NULL` : option.description
                }
                disabled={option.value === "n" && !nullable}
              >
                {option.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </Field>
      <Footer>
        {to && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            className="mr-auto"
            onClick={() => edit.start({ kind: "foreignKey", table: qualifiedName(entity), column })}
          >
            <ArrowLeftIcon />
            Another column
          </Button>
        )}
        <Button type="button" variant="ghost" size="sm" onClick={edit.stop}>
          Cancel
        </Button>
        <Button
          type="submit"
          variant="primary"
          size="sm"
          data-autofocus={to && plan?.ok ? "" : undefined}
          disabled={!plan?.ok}
        >
          Add foreign key
        </Button>
      </Footer>
    </form>
  );
}

// ---- A failed edit --------------------------------------------------------------------

function FailureNote({
  edit,
  table,
  column,
}: {
  edit: EditContextValue;
  table: string;
  column?: string;
}) {
  const key = column === undefined ? table : `${table}\u0000${column}`;
  const mark = useGraphUi((s) => s.marks.get(key));
  if (!mark || mark.status !== "failed") return null;
  return (
    <div className="grid gap-3">
      <div className="flex gap-2">
        <WarningTriangleIcon aria-hidden className="mt-0.5 size-4 shrink-0 text-destructive" />
        <div className="grid gap-0.5">
          <h2 className="text-sm font-semibold text-foreground">{mark.label} failed</h2>
          <p className="text-sm text-muted-foreground">{mark.message}</p>
        </div>
      </div>
      <Footer>
        <Button variant="ghost" size="sm" onClick={edit.stop}>
          Keep it
        </Button>
        {edit.editor.onUndo && (
          <Button
            data-autofocus=""
            size="sm"
            onClick={() => {
              edit.editor.onUndo?.(mark.id);
              edit.stop();
            }}
          >
            Undo it
          </Button>
        )}
      </Footer>
    </div>
  );
}
