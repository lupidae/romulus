// Timelines without the API: a list of models folded into object histories,
// the way the API's timeline does it (objects matched by name, a change when
// a value differs), for the unit tests and the dev route. The API words
// changes with romulus-diff; this says less (the fields that differ).

import type { Column, Entity, Schema } from "@/features/schema/model";

import type { ObjectChange, Timeline, TimelineVersion } from "./history";

export interface FixtureVersion {
  model: Schema;
  message?: string;
  author?: string;
  origin?: TimelineVersion["origin"];
  /** Renames into this version, as a diff reporting them would: tables, and columns (`table.column`). */
  renames?: { tables?: Record<string, string>; columns?: Record<string, string> };
  removed?: boolean;
}

type Value = Record<string, unknown>;
interface Track {
  key: string;
  states: { from: number; to: number | null; value: Value; changes: ObjectChange[] }[];
}

/** Fields whose change a fixture words (the rest only change the value). */
const WATCHED = ["type", "not_null", "default", "comment", "kind", "definition", "labels"];

function changesOf(before: Value, after: Value): ObjectChange[] {
  const out: ObjectChange[] = [];
  for (const field of WATCHED) {
    if (field in after && JSON.stringify(before[field]) !== JSON.stringify(after[field]))
      out.push({ field, before: before[field], after: after[field] } as unknown as ObjectChange);
  }
  return out;
}

class Tracks {
  items: Track[] = [];
  private byKey = new Map<string, Track>();

  apply(at: number, present: Map<string, Value>, renames: Record<string, string> = {}) {
    const seen = new Set<string>();
    for (const [from, to] of Object.entries(renames)) {
      const track = this.byKey.get(from);
      if (!track || !present.has(to)) continue;
      this.byKey.delete(from);
      this.byKey.set(to, track);
      track.key = to;
      const last = track.states[track.states.length - 1]!;
      last.to = at;
      track.states.push({
        from: at,
        to: null,
        value: present.get(to)!,
        changes: [
          { field: "name", before: from.split(".").pop(), after: to.split(".").pop() },
          ...changesOf(last.value, present.get(to)!),
        ] as ObjectChange[],
      });
      seen.add(to);
    }
    for (const track of this.items) {
      const last = track.states[track.states.length - 1]!;
      if (last.to === null && !present.has(track.key)) last.to = at;
    }
    for (const [key, value] of present) {
      if (seen.has(key)) continue;
      let track = this.byKey.get(key);
      if (!track) {
        track = { key, states: [] };
        this.byKey.set(key, track);
        this.items.push(track);
      }
      const last = track.states[track.states.length - 1];
      if (last && last.to === null) {
        if (JSON.stringify(last.value) === JSON.stringify(value)) continue;
        last.to = at;
        track.states.push({ from: at, to: null, value, changes: changesOf(last.value, value) });
      } else {
        track.states.push({ from: at, to: null, value, changes: [] });
      }
    }
  }
}

const parts = ["columns", "constraints", "indexes", "policies"] as const;

function headOf(e: Entity): Value {
  const { columns, constraints: _c, indexes: _i, policies: _p, ...head } = e;
  return { ...head, column_order: (columns ?? []).map((c: Column) => c.name) };
}

/** The timeline of `versions`, oldest first, numbered from `first`. */
export function timelineFrom(versions: FixtureVersion[], first = 1): Timeline {
  const tables = new Tracks();
  const tableParts = new Map<Track, Record<(typeof parts)[number], Tracks>>();
  const types = new Tracks();
  const partitions = new Tracks();
  const start = Date.UTC(2026, 8, 1);
  const out: TimelineVersion[] = [];
  versions.forEach((v, at) => {
    const entities = new Map(
      (v.model.entities ?? []).map((e) => [`${e.schema}.${e.name}`, e] as const),
    );
    tables.apply(at, new Map([...entities].map(([k, e]) => [k, headOf(e)])), v.renames?.tables);
    for (const track of tables.items) {
      const entity = entities.get(track.key as `${string}.${string}`);
      let own = tableParts.get(track);
      if (!own) {
        own = {
          columns: new Tracks(),
          constraints: new Tracks(),
          indexes: new Tracks(),
          policies: new Tracks(),
        };
        tableParts.set(track, own);
      }
      for (const kind of parts) {
        const list = (entity?.[kind] ?? []) as { name: string }[];
        const renames =
          kind === "columns"
            ? Object.fromEntries(
                Object.entries(v.renames?.columns ?? {})
                  .filter(([from]) => from.startsWith(`${track.key}.`))
                  .map(([from, to]) => [from.slice(track.key.length + 1), to]),
              )
            : undefined;
        own[kind].apply(at, new Map(list.map((x) => [x.name, x as unknown as Value])), renames);
      }
    }
    types.apply(
      at,
      new Map([
        ...(v.model.enums ?? []).map(
          (t) => [`enum:${t.schema}.${t.name}`, t as unknown as Value] as const,
        ),
        ...(v.model.domains ?? []).map(
          (t) => [`domain:${t.schema}.${t.name}`, t as unknown as Value] as const,
        ),
        ...(v.model.composites ?? []).map(
          (t) => [`composite:${t.schema}.${t.name}`, t as unknown as Value] as const,
        ),
      ]),
    );
    partitions.apply(
      at,
      new Map(
        (v.model.partitions ?? []).map((p) => [
          `${p.child_schema}.${p.child}`,
          p as unknown as Value,
        ]),
      ),
    );
    const number = first + at;
    out.push({
      id: `00000000-0000-4000-8000-${String(number).padStart(12, "0")}`,
      number,
      origin: v.origin ?? "push",
      source_kind: "remus_json",
      message: v.message ?? null,
      created_at: new Date(start + at * 86_400_000).toISOString(),
      author: v.author ? { kind: "user", id: `user-${v.author}`, name: v.author } : null,
      written_migration: false,
      postgres: {
        server_version: "17.6",
        major: 17,
        database_major: 17,
        other_major: false,
      },
      removed_at: v.removed ? new Date(start + at * 86_400_000 + 3_600_000).toISOString() : null,
      removal_reason: v.removed ? "Pushed from staging" : null,
    });
  });

  const split = (key: string) => {
    const dot = key.indexOf(".");
    return { schema: key.slice(0, dot), name: key.slice(dot + 1) };
  };
  return {
    versions: out,
    earlier: 0,
    history: {
      tables: tables.items
        .map((t) => {
          const own = tableParts.get(t)!;
          const list = (kind: (typeof parts)[number]) =>
            own[kind].items.map((p) => ({ id: p.key, states: p.states }));
          return {
            id: t.key,
            ...split(t.key),
            states: t.states,
            columns: list("columns"),
            constraints: list("constraints"),
            indexes: list("indexes"),
            policies: list("policies"),
          };
        })
        .sort((a, b) => a.id.localeCompare(b.id)),
      types: types.items.map((t) => {
        const [kind, rest] = t.key.split(":") as ["enum" | "domain" | "composite", string];
        return { kind, ...split(rest), states: t.states };
      }),
      partitions: partitions.items.map((p) => ({ ...split(p.key), states: p.states })),
      extensions: [],
    },
    layout: { layout: {}, groups: {} },
  } as unknown as Timeline;
}

/** A column as remus writes it. */
export function column(name: string, type = "bigint", extra: Partial<Column> = {}): Column {
  return {
    name,
    type,
    type_schema: "pg_catalog",
    type_name: type,
    type_kind: "b",
    not_null: false,
    default: null,
    identity: null,
    generated: null,
    comment: null,
    ...extra,
  };
}

/** A table of `columns` (`id` first, its primary key), referencing others by `<table>_id` columns. */
export function table(name: string, columns: Column[], refs: string[] = []): Entity {
  return {
    schema: "public",
    name,
    kind: "r",
    comment: null,
    partition_key: null,
    view_definition: null,
    columns: [column("id"), ...columns],
    constraints: [
      {
        name: `${name}_pkey`,
        kind: "p",
        columns: ["id"],
        ref_schema: null,
        ref_table: null,
        on_update: null,
        on_delete: null,
        definition: "PRIMARY KEY (id)",
        comment: null,
      },
      ...refs.map((target) => ({
        name: `${name}_${target}_id_fkey`,
        kind: "f" as const,
        columns: [`${target}_id`],
        ref_schema: "public",
        ref_table: target,
        ref_columns: ["id"],
        on_update: "a" as const,
        on_delete: "a" as const,
        definition: `FOREIGN KEY (${target}_id) REFERENCES public.${target}(id)`,
        comment: null,
      })),
    ],
  };
}

export function model(entities: Entity[]): Schema {
  return { remus_version: "0.2", database: "shop", server_version: "17.6", entities };
}

/**
 * A long history for the dev route and the perf spec: `tables` tables grown
 * over `versions` versions, with columns added, retyped and dropped, a few
 * tables dropped, and one rename.
 */
export function largeHistory(tables = 220, versions = 120): Timeline {
  const out: FixtureVersion[] = [];
  const authors = ["Ada", "Grace", "Linus", "Barbara"];
  for (let v = 0; v < versions; v++) {
    const count = Math.round(tables * (0.4 + (0.6 * v) / Math.max(1, versions - 1)));
    const entities: Entity[] = [];
    for (let t = 0; t < count; t++) {
      if (t % 37 === 5 && v > versions * 0.7) continue; // dropped late
      const columns: Column[] = [column("name", "text"), column("created_at", "timestamptz")];
      if (v > (t * 7) % versions) columns.push(column("status", "text"));
      if (v > (t * 13) % versions && t % 3 === 0)
        columns.push(column("amount", v > ((t * 13) % versions) + 5 ? "numeric" : "bigint"));
      if (t % 5 === 0 && v < (t * 11) % versions) columns.push(column("legacy_code", "text"));
      const refs = t > 0 && t % 4 !== 0 ? [`table_${Math.floor((t * 7) / 11) % t}`] : [];
      for (const r of refs) columns.push(column(`${r}_id`));
      entities.push(table(`table_${t}`, columns, refs));
    }
    out.push({
      model: model(entities),
      message: v === 0 ? "Initial schema" : `Change ${v}`,
      author: authors[v % authors.length],
      origin: v % 5 === 0 ? "draft" : v % 7 === 0 ? "import" : "push",
    });
  }
  return timelineFrom(out);
}

/**
 * A shop's schema over ten versions, for the dev route: tables added and
 * dropped, columns added, retyped, made not null, a table and a column
 * renamed (as a diff reporting renames would say), from three people.
 */
export function shopHistory({ removed = false } = {}): Timeline {
  const customers = (extra: Column[] = []) =>
    table("customers", [column("name", "text"), column("email", "text"), ...extra]);
  const orders = (extra: Column[] = []) =>
    table("orders", [column("customers_id"), column("total", "bigint"), ...extra], ["customers"]);
  const products = (name = "products") =>
    table(name, [column("title", "text"), column("price", "numeric")]);
  const lines = (products = "products") =>
    table(
      "order_lines",
      [column("orders_id"), column(`${products}_id`), column("quantity", "integer")],
      ["orders", products],
    );
  const steps: FixtureVersion[] = [
    { model: model([customers(), orders()]), message: "Customers and orders", author: "Ada" },
    {
      model: model([customers(), orders(), products()]),
      message: "A product catalog",
      author: "Grace",
      origin: "draft",
    },
    {
      model: model([customers(), orders(), products(), lines(), table("coupons", [])]),
      message: "Order lines, coupons",
      author: "Grace",
      origin: "draft",
    },
    {
      model: model([
        customers([column("created_at", "timestamptz", { not_null: true, default: "now()" })]),
        orders(),
        products(),
        lines(),
        table("coupons", [column("code", "text")]),
      ]),
      message: "Timestamps",
      author: "Linus",
    },
    {
      model: model([
        customers([column("created_at", "timestamptz", { not_null: true, default: "now()" })]),
        orders([column("status", "text")]),
        products(),
        lines(),
      ]),
      message: "Drop coupons, order status",
      author: "Ada",
      removed,
    },
    {
      model: model([
        customers([column("created_at", "timestamptz", { not_null: true, default: "now()" })]),
        table(
          "orders",
          [column("customers_id"), column("total", "numeric"), column("status", "text")],
          ["customers"],
        ),
        products(),
        lines(),
      ]),
      message: "Totals as numeric",
      author: "Grace",
      origin: "import",
    },
    {
      model: model([
        customers([column("created_at", "timestamptz", { not_null: true, default: "now()" })]),
        table(
          "orders",
          [column("customers_id"), column("total", "numeric"), column("status", "text")],
          ["customers"],
        ),
        products("catalog_items"),
        lines("catalog_items"),
      ]),
      message: "Products become catalog items",
      author: "Linus",
      origin: "draft",
      renames: {
        tables: { "public.products": "public.catalog_items" },
        columns: { "public.order_lines.products_id": "catalog_items_id" },
      },
    },
    {
      model: model([
        table("customers", [
          column("name", "text"),
          column("email_address", "text"),
          column("created_at", "timestamptz", { not_null: true, default: "now()" }),
        ]),
        table(
          "orders",
          [column("customers_id"), column("total", "numeric"), column("status", "text")],
          ["customers"],
        ),
        products("catalog_items"),
        lines("catalog_items"),
        table("invoices", [column("orders_id"), column("issued_at", "timestamptz")], ["orders"]),
      ]),
      message: "Invoices",
      author: "Ada",
      renames: { columns: { "public.customers.email": "email_address" } },
    },
  ];
  return timelineFrom(steps);
}
