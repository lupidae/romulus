import { useStore } from "@xyflow/react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

import { cn } from "@/lib/utils";

import { STATUS_LABEL } from "../diff";
import { type Column, qualifiedName, referentialAction } from "../model";
import type { ColumnModel, GraphModel, TableModel, TypeModel } from "./build";
import { describeCardinality } from "./cardinality";
import { StatusBadge } from "./status";
import { KIND_LABEL } from "./kind";

type Target = { table: TableModel; column?: ColumnModel; rect: DOMRect; transform: unknown };

const OPEN_DELAY = 450;
const CARD_WIDTH = 304;

/**
 * Details of the column (or table header) under the pointer: full type, enum
 * labels or domain definition, default, identity, what it references, the
 * comment, and in diff mode what it was. One card for the whole graph, found
 * by event delegation, so 300 tables do not mount 3,000 tooltips.
 */
export function HoverDetails({
  container,
  graph,
}: {
  container: React.RefObject<HTMLElement | null>;
  graph: GraphModel;
}) {
  const [target, setTarget] = useState<Target | null>(null);
  const openTimer = useRef<number | undefined>(undefined);
  const isOpen = useRef(false);
  const transform = useStore((s) => s.transform);
  const transformRef = useRef(transform);
  useEffect(() => {
    transformRef.current = transform;
    isOpen.current = target !== null && target.transform === transform;
  });

  useEffect(() => {
    const el = container.current;
    if (!el) return;
    let current: Element | null = null;
    const find = (node: EventTarget | null) => {
      if (!(node instanceof Element)) return null;
      const row = node.closest("[data-column], [data-table-header]");
      const flowNode = row?.closest<HTMLElement>(".react-flow__node");
      const table = flowNode?.dataset.id ? graph.byKey.get(flowNode.dataset.id) : undefined;
      if (!row || !table) return null;
      const name = row.getAttribute("data-column");
      return {
        row,
        table,
        column: name === null ? undefined : table.columns.find((c) => c.name === name),
      };
    };
    const over = (event: PointerEvent) => {
      if (event.pointerType === "touch" || event.buttons) return;
      const hit = find(event.target);
      if (hit?.row === current) return;
      current = hit?.row ?? null;
      window.clearTimeout(openTimer.current);
      if (!hit) {
        openTimer.current = window.setTimeout(() => setTarget(null), 120);
        return;
      }
      const show = () =>
        setTarget({
          table: hit.table,
          column: hit.column,
          rect: hit.row.getBoundingClientRect(),
          transform: transformRef.current,
        });
      // Moving between rows with a card open switches at once.
      if (isOpen.current) show();
      else openTimer.current = window.setTimeout(show, OPEN_DELAY);
    };
    const leave = () => {
      current = null;
      window.clearTimeout(openTimer.current);
      setTarget(null);
    };
    el.addEventListener("pointerover", over);
    el.addEventListener("pointerleave", leave);
    el.addEventListener("pointerdown", leave);
    el.addEventListener("wheel", leave, { passive: true });
    return () => {
      window.clearTimeout(openTimer.current);
      el.removeEventListener("pointerover", over);
      el.removeEventListener("pointerleave", leave);
      el.removeEventListener("pointerdown", leave);
      el.removeEventListener("wheel", leave);
    };
  }, [container, graph]);

  // Any pan or zoom since it opened has moved the row away from the card.
  if (!target || target.transform !== transform) return null;
  return createPortal(<Card target={target} graph={graph} />, document.body);
}

function Card({ target, graph }: { target: Target; graph: GraphModel }) {
  const ref = useRef<HTMLDivElement>(null);
  const [style, setStyle] = useState<React.CSSProperties>({ visibility: "hidden" });

  useLayoutEffect(() => {
    const card = ref.current;
    if (!card) return;
    const { rect } = target;
    const height = card.offsetHeight;
    const right = rect.right + 8;
    const left = right + CARD_WIDTH > window.innerWidth - 8 ? rect.left - CARD_WIDTH - 8 : right;
    const top = Math.max(8, Math.min(rect.top - 6, window.innerHeight - height - 8));
    setStyle({ left: Math.max(8, left), top, width: CARD_WIDTH });
  }, [target]);

  return (
    <div
      ref={ref}
      role="tooltip"
      style={style}
      className={cn(
        "pointer-events-none fixed z-(--z-tooltip) grid gap-2.5 rounded-lg border border-border bg-popover p-3 text-popover-foreground shadow-popover",
        "animate-in duration-(--duration-fast) fade-in-0 zoom-in-[0.98]",
      )}
    >
      {target.column ? (
        <ColumnDetails table={target.table} column={target.column} graph={graph} />
      ) : (
        <TableDetails table={target.table} />
      )}
    </div>
  );
}

function Row({
  label,
  children,
  mono = true,
}: {
  label: string;
  children: React.ReactNode;
  mono?: boolean;
}) {
  return (
    <>
      <dt className="text-xs text-muted-foreground">{label}</dt>
      <dd className={cn("min-w-0 text-xs break-words text-foreground", mono && "font-mono")}>
        {children}
      </dd>
    </>
  );
}

function definition(
  c: Pick<Column, "type" | "not_null" | "default" | "identity" | "generated">,
): string {
  const parts = [c.type];
  if (c.identity)
    parts.push(`GENERATED ${c.identity === "a" ? "ALWAYS" : "BY DEFAULT"} AS IDENTITY`);
  if (c.generated)
    parts.push(`GENERATED ALWAYS AS (${c.default}) ${c.generated === "s" ? "STORED" : "VIRTUAL"}`);
  else if (c.default) parts.push(`DEFAULT ${c.default}`);
  if (c.not_null) parts.push("NOT NULL");
  return parts.join(" ");
}

function ColumnDetails({
  table,
  column: c,
  graph,
}: {
  table: TableModel;
  column: ColumnModel;
  graph: GraphModel;
}) {
  const type = c.userType ? graph.types.get(qualifiedName(c.userType)) : undefined;
  const fks = graph.edges.filter(
    (e) => e.kind === "fk" && e.source === table.key && e.columns.includes(c.name),
  );
  const referencedBy = graph.edges.filter(
    (e) => e.kind === "fk" && e.target === table.key && e.refColumns.includes(c.name),
  );
  const facts = [
    c.pk && "Primary key",
    c.unique && "Unique",
    c.notNull ? "Not null" : "Nullable",
    c.identity && `Identity, ${c.identity}`,
    c.generated && `Generated, ${c.generated}`,
  ].filter(Boolean);

  return (
    <>
      <div className="flex items-baseline gap-2">
        <span className="min-w-0 truncate font-mono text-sm font-semibold">{c.name}</span>
        <span className="ml-auto shrink-0 font-mono text-xs text-syntax-type">{c.type}</span>
      </div>
      <p className="text-xs text-muted-foreground">{facts.join(" · ")}</p>
      {c.comment && <p className="text-sm text-foreground">{c.comment}</p>}
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
        {c.default && !c.generated && <Row label="Default">{c.default}</Row>}
        {c.generated && <Row label="Expression">{c.default}</Row>}
        {fks.map((e) => (
          <Row key={e.id} label="References">
            {e.target}({e.refColumns.join(", ")})
            {referentialAction(e.onDelete) && (
              <span className="text-muted-foreground">
                {" "}
                on delete {referentialAction(e.onDelete)}
              </span>
            )}
          </Row>
        ))}
        {referencedBy.length > 0 && (
          <Row label="Referenced by">
            {referencedBy
              .slice(0, 4)
              .map((e) => e.source)
              .join(", ")}
            {referencedBy.length > 4 && ` and ${referencedBy.length - 4} more`}
          </Row>
        )}
      </dl>
      {fks[0]?.cardinality && (
        <p className="text-xs text-muted-foreground">{describeCardinality(fks[0].cardinality)}.</p>
      )}
      {type && <TypeDetails type={type} />}
      {c.status && (
        <div className="grid gap-1.5 border-t border-border pt-2.5">
          <StatusBadge status={c.status} className="justify-self-start" />
          {c.before && (
            <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
              <Row label="Before">{definition(c.before)}</Row>
              <Row label="After">
                {definition({
                  type: c.type,
                  not_null: c.notNull,
                  default: c.default,
                  identity: c.identity === "always" ? "a" : c.identity ? "d" : null,
                  generated: c.generated === "stored" ? "s" : c.generated ? "v" : null,
                })}
              </Row>
              {c.before.comment !== c.comment && (
                <Row label="Comment" mono={false}>
                  {c.before.comment ?? "none"} → {c.comment ?? "none"}
                </Row>
              )}
            </dl>
          )}
        </div>
      )}
    </>
  );
}

function TypeDetails({ type }: { type: TypeModel }) {
  return (
    <div className="grid gap-1.5 border-t border-border pt-2.5">
      <p className="text-xs text-muted-foreground">
        {type.kind === "enum" ? "Enum" : type.kind === "domain" ? "Domain" : "Composite type"}{" "}
        <span className="font-mono text-foreground">
          {type.schema}.{type.name}
        </span>
        {type.status && ` · ${STATUS_LABEL[type.status].toLowerCase()}`}
      </p>
      {type.kind === "enum" ? (
        <ul className="flex flex-wrap gap-1">
          {type.lines.map((label) => (
            <li
              key={label}
              className="rounded-sm bg-muted px-1.5 font-mono text-xs leading-5 text-foreground"
            >
              {label}
            </li>
          ))}
        </ul>
      ) : (
        <ul className="grid gap-0.5 font-mono text-xs text-foreground">
          {type.lines.map((line) => (
            <li key={line} className="break-words">
              {line}
            </li>
          ))}
        </ul>
      )}
      {type.comment && <p className="text-xs text-muted-foreground">{type.comment}</p>}
    </div>
  );
}

function TableDetails({ table }: { table: TableModel }) {
  const e = table.entity;
  const checks = (e.constraints ?? []).filter((c) => c.kind === "c").length;
  const facts = [
    KIND_LABEL[table.kind] ?? "table",
    `${table.columns.length} ${table.columns.length === 1 ? "column" : "columns"}`,
    (e.indexes ?? []).length > 0 &&
      `${e.indexes!.length} ${e.indexes!.length === 1 ? "index" : "indexes"}`,
    checks > 0 && `${checks} ${checks === 1 ? "check" : "checks"}`,
    table.rls && "row level security",
    e.unlogged && "unlogged",
  ].filter(Boolean);
  return (
    <>
      <div className="flex items-center gap-2">
        <span className="min-w-0 truncate font-mono text-sm font-semibold">{table.key}</span>
        {table.status && <StatusBadge status={table.status} className="ml-auto" />}
      </div>
      <p className="text-xs text-muted-foreground">{facts.join(" · ")}</p>
      {table.comment && <p className="text-sm text-foreground">{table.comment}</p>}
      {(table.partitionKey || (e.policies ?? []).length > 0) && (
        <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
          {table.partitionKey && <Row label="Partition by">{table.partitionKey}</Row>}
          {(e.policies ?? []).length > 0 && (
            <Row label="Policies">{e.policies!.map((p) => p.name).join(", ")}</Row>
          )}
        </dl>
      )}
      {e.view_definition && (
        <pre className="max-h-32 overflow-hidden rounded-md bg-muted px-2 py-1.5 font-mono text-xs whitespace-pre-wrap text-foreground">
          {e.view_definition.trim()}
        </pre>
      )}
    </>
  );
}
