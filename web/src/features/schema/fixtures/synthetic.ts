// Synthetic schemas for the dev viewer and tests: a mid-sized SaaS schema with
// groups, and a seeded generator for the 300-table performance fixture. Both
// are remus models built in code (a JSON file of 300 tables would weigh a
// megabyte), plus a small renderer for their SQL. Not product code: real SQL
// comes from remus-sql through the API.

import type { Groups } from "../layout";
import type { Column, Constraint, Entity, EnumType, Partition, Schema } from "../model";

// ---- Builders -------------------------------------------------------------

const TYPES: Record<string, [type: string, typeName: string]> = {
  bigint: ["bigint", "int8"],
  int: ["integer", "int4"],
  smallint: ["smallint", "int2"],
  text: ["text", "text"],
  bool: ["boolean", "bool"],
  ts: ["timestamp with time zone", "timestamptz"],
  date: ["date", "date"],
  uuid: ["uuid", "uuid"],
  json: ["jsonb", "jsonb"],
  numeric: ["numeric(12,2)", "numeric"],
  inet: ["inet", "inet"],
  varchar: ["character varying(255)", "varchar"],
  "text[]": ["text[]", "text"],
};

interface ColumnSpec {
  column: Column;
  fk?: { table: string; nullable: boolean };
  unique?: boolean;
  pk?: boolean;
}

/**
 * One column from a compact spec, `name type[!] [flags]`:
 * `id pk` (bigint identity primary key), `org_id -> identity.organizations`
 * (`?->` for a nullable reference), `email text! unique`, `status order_status!`
 * (an enum of the same schema), `total numeric! =0`, `slug text gen`.
 */
function parseColumn(spec: string, schema: string, enums: Set<string>): ColumnSpec {
  const [name, second = "text", ...rest] = spec.split(" ");
  const base: Column = {
    name: name!,
    type: "bigint",
    type_schema: "pg_catalog",
    type_name: "int8",
    type_kind: "b",
    is_array: false,
    not_null: false,
    default: null,
    identity: null,
    generated: null,
    comment: null,
  };
  if (second === "pk") {
    return { column: { ...base, not_null: true, identity: "a" }, pk: true };
  }
  if (second === "->" || second === "?->") {
    const target = rest[0]!;
    return {
      column: { ...base, not_null: second === "->" },
      fk: {
        table: target.includes(".") ? target : `${schema}.${target}`,
        nullable: second === "?->",
      },
      unique: rest.includes("unique"),
    };
  }
  const notNull = second.endsWith("!");
  const key = notNull ? second.slice(0, -1) : second;
  const column: Column = { ...base, not_null: notNull };
  if (enums.has(key)) {
    Object.assign(column, { type: key, type_schema: schema, type_name: key, type_kind: "e" });
  } else {
    const [type, typeName] = TYPES[key] ?? ["text", "text"];
    Object.assign(column, { type, type_name: typeName, is_array: key.endsWith("[]") });
  }
  for (const flag of rest) {
    if (flag.startsWith("=")) column.default = flag.slice(1);
    if (flag === "now") column.default = "now()";
    if (flag === "gen") {
      column.generated = "s";
      column.default = `lower(${name!.replace(/_key$/, "")})`;
    }
  }
  return { column, unique: rest.includes("unique") };
}

const constraint = (
  c: Omit<Constraint, "ref_schema" | "ref_table" | "on_update" | "on_delete" | "comment"> &
    Partial<Constraint>,
): Constraint => ({
  ref_schema: null,
  ref_table: null,
  on_update: null,
  on_delete: null,
  comment: null,
  ...c,
});

interface TableSpec {
  name: string;
  columns: string[];
  comment?: string;
  /** Column comments, by column name. */
  notes?: Record<string, string>;
  unique?: string[][];
  /** Composite primary key (instead of an `id pk` column). */
  primaryKey?: string[];
  kind?: "r" | "p" | "v" | "m";
  partitionKey?: string;
  dependsOn?: string[];
  rls?: boolean;
}

function buildEntity(schema: string, spec: TableSpec, enums: Set<string>): Entity {
  const parsed = spec.columns.map((c) => parseColumn(c, schema, enums));
  const constraints: Constraint[] = [];
  const pkColumns = spec.primaryKey ?? parsed.filter((p) => p.pk).map((p) => p.column.name);
  const isTable = spec.kind !== "v" && spec.kind !== "m";
  if (isTable && pkColumns.length) {
    constraints.push(
      constraint({
        name: `${spec.name}_pkey`,
        kind: "p",
        columns: pkColumns,
        definition: `PRIMARY KEY (${pkColumns.join(", ")})`,
      }),
    );
  }
  for (const p of parsed) {
    if (!isTable) continue;
    if (p.unique) {
      constraints.push(
        constraint({
          name: `${spec.name}_${p.column.name}_key`,
          kind: "u",
          columns: [p.column.name],
          definition: `UNIQUE (${p.column.name})`,
        }),
      );
    }
    if (p.fk) {
      const [refSchema, refTable] = p.fk.table.split(".") as [string, string];
      const onDelete = p.fk.nullable ? "n" : "c";
      constraints.push(
        constraint({
          name: `${spec.name}_${p.column.name}_fkey`,
          kind: "f",
          columns: [p.column.name],
          ref_schema: refSchema,
          ref_table: refTable,
          ref_columns: ["id"],
          on_update: "a",
          on_delete: onDelete,
          definition: `FOREIGN KEY (${p.column.name}) REFERENCES ${refSchema}.${refTable}(id) ON DELETE ${onDelete === "c" ? "CASCADE" : "SET NULL"}`,
        }),
      );
    }
  }
  for (const cols of spec.unique ?? []) {
    constraints.push(
      constraint({
        name: `${spec.name}_${cols.join("_")}_key`,
        kind: "u",
        columns: cols,
        definition: `UNIQUE (${cols.join(", ")})`,
      }),
    );
  }
  return {
    schema,
    name: spec.name,
    kind: spec.kind ?? "r",
    comment: spec.comment ?? null,
    rls: spec.rls ?? false,
    rls_forced: false,
    unlogged: false,
    partition_key: spec.partitionKey ?? null,
    view_definition: isTable
      ? null
      : ` SELECT * FROM ${(spec.dependsOn ?? [])[0] ?? "pg_catalog.pg_class"};`,
    depends_on: (spec.dependsOn ?? []).map((d) => {
      const [s, n] = d.includes(".") ? d.split(".") : [schema, d];
      return { schema: s!, name: n! };
    }),
    columns: parsed.map((p) => ({ ...p.column, comment: spec.notes?.[p.column.name] ?? null })),
    constraints,
    indexes: parsed
      .filter((p) => p.fk && isTable)
      .map((p) => ({
        name: `${spec.name}_${p.column.name}_idx`,
        unique: false,
        partial: false,
        definition: `CREATE INDEX ${spec.name}_${p.column.name}_idx ON ${schema}.${spec.name} USING btree (${p.column.name})`,
      })),
    policies: spec.rls
      ? [
          {
            name: `${spec.name}_tenant`,
            command: "*",
            permissive: true,
            roles: [],
            using: "(org_id = current_setting('app.org_id')::bigint)",
            check: null,
          },
        ]
      : [],
  };
}

function schemaOf(
  database: string,
  entities: Entity[],
  enums: EnumType[],
  partitions: Partition[] = [],
): Schema {
  return {
    remus_version: "0.1.1",
    generated_at: "2026-10-15T08:00:00+00:00",
    database,
    server_version: "17.6",
    entities: [...entities].sort((a, b) =>
      (a.schema + "." + a.name).localeCompare(b.schema + "." + b.name),
    ),
    enums,
    domains: [],
    composites: [],
    partitions,
    extensions: [],
  };
}

// ---- Medium: a SaaS commerce schema, grouped -------------------------------

const ACME_ENUMS: EnumType[] = [
  { schema: "public", name: "member_role", labels: ["owner", "admin", "member"], comment: null },
  {
    schema: "public",
    name: "order_status",
    labels: ["cart", "placed", "paid", "shipped", "delivered", "cancelled"],
    comment: "Lifecycle of an order",
  },
  {
    schema: "public",
    name: "invoice_status",
    labels: ["draft", "open", "paid", "void", "uncollectible"],
    comment: null,
  },
  {
    schema: "public",
    name: "ticket_priority",
    labels: ["low", "normal", "high", "urgent"],
    comment: null,
  },
];

const ACME: Record<string, TableSpec[]> = {
  identity: [
    {
      name: "organizations",
      columns: [
        "id pk",
        "name text!",
        "slug text! unique",
        "plan_id ?-> plans",
        "created_at ts! now",
      ],
      comment: "A customer account; every row of the app belongs to one",
    },
    {
      name: "users",
      columns: [
        "id pk",
        "email text! unique",
        "name text!",
        "avatar_url text",
        "last_seen_at ts",
        "created_at ts! now",
      ],
      notes: { email: "Lowercased on write" },
    },
    {
      name: "memberships",
      columns: [
        "org_id -> organizations",
        "user_id -> users",
        "role member_role! ='member'",
        "joined_at ts! now",
      ],
      primaryKey: ["org_id", "user_id"],
    },
    {
      name: "sessions",
      columns: ["id pk", "user_id -> users", "ip inet", "user_agent text", "expires_at ts!"],
    },
    {
      name: "api_tokens",
      columns: [
        "id pk",
        "org_id -> organizations",
        "name text!",
        "token_hash text! unique",
        "last_used_at ts",
        "revoked_at ts",
      ],
    },
    {
      name: "invitations",
      columns: [
        "id pk",
        "org_id -> organizations",
        "email text!",
        "role member_role!",
        "invited_by ?-> users",
        "expires_at ts!",
      ],
    },
    {
      name: "audit_log",
      columns: [
        "id pk",
        "org_id -> organizations",
        "actor_id ?-> users",
        "action text!",
        "payload json!",
        "at ts! now",
      ],
      rls: true,
      comment: "Append-only; read by the security team",
    },
  ],
  catalog: [
    {
      name: "products",
      columns: [
        "id pk",
        "org_id -> organizations",
        "sku text! unique",
        "name text!",
        "description text",
        "category_id ?-> categories",
        "supplier_id ?-> suppliers",
        "archived bool! =false",
      ],
    },
    {
      name: "product_variants",
      columns: [
        "id pk",
        "product_id -> products",
        "sku text! unique",
        "options json! ='{}'",
        "weight_grams int",
      ],
    },
    {
      name: "categories",
      columns: ["id pk", "parent_id ?-> categories", "name text!", "slug text! unique"],
    },
    {
      name: "prices",
      columns: [
        "id pk",
        "variant_id -> product_variants",
        "currency text! ='EUR'",
        "amount numeric!",
        "valid_from ts! now",
        "valid_to ts",
      ],
    },
    {
      name: "warehouses",
      columns: ["id pk", "name text!", "country text!", "address_id ?-> addresses"],
    },
    {
      name: "inventory_levels",
      columns: [
        "variant_id -> product_variants",
        "warehouse_id -> warehouses",
        "on_hand int! =0",
        "reserved int! =0",
        "updated_at ts! now",
      ],
      primaryKey: ["variant_id", "warehouse_id"],
    },
    {
      name: "suppliers",
      columns: ["id pk", "name text!", "email text", "lead_time_days smallint"],
    },
  ],
  orders: [
    {
      name: "customers",
      columns: [
        "id pk",
        "org_id -> organizations",
        "email text!",
        "name text",
        "user_id ?-> users unique",
        "marketing_opt_in bool! =false",
      ],
      unique: [["org_id", "email"]],
    },
    {
      name: "addresses",
      columns: [
        "id pk",
        "customer_id ?-> customers",
        "line1 text!",
        "line2 text",
        "city text!",
        "postcode text!",
        "country text!",
      ],
    },
    {
      name: "carts",
      columns: [
        "id pk",
        "customer_id ?-> customers",
        "session_id ?-> sessions",
        "updated_at ts! now",
      ],
    },
    {
      name: "cart_items",
      columns: ["cart_id -> carts", "variant_id -> product_variants", "quantity int! =1"],
      primaryKey: ["cart_id", "variant_id"],
    },
    {
      name: "orders",
      columns: [
        "id pk",
        "org_id -> organizations",
        "customer_id -> customers",
        "status order_status! ='cart'",
        "shipping_address_id ?-> addresses",
        "billing_address_id ?-> addresses",
        "total numeric! =0",
        "placed_at ts",
      ],
      comment: "One checkout; totals are frozen when placed",
      rls: true,
    },
    {
      name: "order_items",
      columns: [
        "id pk",
        "order_id -> orders",
        "variant_id -> product_variants",
        "quantity int!",
        "unit_price numeric!",
      ],
    },
    {
      name: "shipments",
      columns: [
        "id pk",
        "order_id -> orders",
        "warehouse_id -> warehouses",
        "carrier text",
        "tracking_number text",
        "shipped_at ts",
      ],
    },
    {
      name: "shipment_items",
      columns: ["shipment_id -> shipments", "order_item_id -> order_items"],
      primaryKey: ["shipment_id", "order_item_id"],
    },
    { name: "returns", columns: ["id pk", "order_id -> orders", "reason text", "received_at ts"] },
  ],
  billing: [
    {
      name: "plans",
      columns: [
        "id pk",
        "name text! unique",
        "monthly_price numeric!",
        "seats int",
        "features text[]! ='{}'",
      ],
    },
    {
      name: "subscriptions",
      columns: [
        "id pk",
        "org_id -> organizations unique",
        "plan_id -> plans",
        "trial_ends_at ts",
        "renews_at ts!",
        "cancelled_at ts",
      ],
    },
    {
      name: "invoices",
      columns: [
        "id pk",
        "org_id -> organizations",
        "subscription_id ?-> subscriptions",
        "order_id ?-> orders",
        "number text! unique",
        "status invoice_status! ='draft'",
        "issued_at ts",
        "due_at ts",
      ],
    },
    {
      name: "invoice_lines",
      columns: [
        "id pk",
        "invoice_id -> invoices",
        "description text!",
        "quantity int! =1",
        "amount numeric!",
        "tax_rate_id ?-> tax_rates",
      ],
    },
    {
      name: "payment_methods",
      columns: [
        "id pk",
        "customer_id -> customers",
        "brand text!",
        "last4 text!",
        "expires_on date",
      ],
    },
    {
      name: "payments",
      columns: [
        "id pk",
        "invoice_id -> invoices",
        "payment_method_id ?-> payment_methods",
        "amount numeric!",
        "succeeded_at ts",
        "failure_reason text",
      ],
    },
    {
      name: "refunds",
      columns: [
        "id pk",
        "payment_id -> payments",
        "return_id ?-> returns unique",
        "amount numeric!",
        "refunded_at ts! now",
      ],
    },
    {
      name: "coupons",
      columns: ["id pk", "code text! unique", "percent_off smallint", "expires_at ts"],
    },
    {
      name: "coupon_redemptions",
      columns: ["coupon_id -> coupons", "order_id -> orders unique", "redeemed_at ts! now"],
      primaryKey: ["coupon_id", "order_id"],
    },
    {
      name: "tax_rates",
      columns: ["id pk", "country text!", "rate numeric!", "inclusive bool! =false"],
    },
  ],
  support: [
    {
      name: "tickets",
      columns: [
        "id pk",
        "org_id -> organizations",
        "customer_id ?-> customers",
        "assignee_id ?-> users",
        "subject text!",
        "priority ticket_priority! ='normal'",
        "opened_at ts! now",
        "closed_at ts",
      ],
    },
    {
      name: "ticket_messages",
      columns: [
        "id pk",
        "ticket_id -> tickets",
        "author_id ?-> users",
        "body text!",
        "internal bool! =false",
        "sent_at ts! now",
      ],
    },
    {
      name: "tags",
      columns: ["id pk", "org_id -> organizations", "name text!"],
      unique: [["org_id", "name"]],
    },
    {
      name: "ticket_tags",
      columns: ["ticket_id -> tickets", "tag_id -> tags"],
      primaryKey: ["ticket_id", "tag_id"],
    },
    {
      name: "knowledge_articles",
      columns: [
        "id pk",
        "org_id -> organizations",
        "title text!",
        "body text!",
        "published_at ts",
        "author_id ?-> users",
      ],
    },
    {
      name: "csat_responses",
      columns: ["id pk", "ticket_id -> tickets unique", "score smallint!", "comment text"],
    },
  ],
  analytics: [
    {
      name: "events",
      columns: [
        "id pk",
        "org_id -> organizations",
        "user_id ?-> users",
        "name text!",
        "properties json! ='{}'",
        "occurred_at ts!",
      ],
      kind: "p",
      partitionKey: "RANGE (occurred_at)",
      primaryKey: ["id", "occurred_at"],
      comment: "Product analytics, one partition per month",
    },
    {
      name: "page_views",
      columns: [
        "id pk",
        "session_id ?-> sessions",
        "path text!",
        "referrer text",
        "viewed_at ts! now",
      ],
    },
    {
      name: "daily_revenue",
      columns: ["day date", "org_id bigint", "revenue numeric"],
      kind: "m",
      dependsOn: ["orders", "payments"],
    },
    {
      name: "order_exports",
      columns: [
        "id pk",
        "order_id -> orders",
        "exported_at ts! now",
        ...[
          "customer_email",
          "customer_name",
          "billing_line1",
          "billing_city",
          "billing_postcode",
          "billing_country",
          "shipping_line1",
          "shipping_city",
          "shipping_postcode",
          "shipping_country",
          "currency",
          "subtotal",
          "discount",
          "tax",
          "shipping_fee",
          "total",
          "payment_brand",
          "payment_last4",
          "coupon_code",
          "channel",
          "utm_source",
          "utm_campaign",
          "warehouse_code",
          "carrier",
          "tracking_number",
          "notes",
        ].map((c) => `${c} ${/total|subtotal|discount|tax|fee/.test(c) ? "numeric" : "text"}`),
      ],
      comment: "Flat rows for the finance export, rebuilt nightly",
    },
    {
      name: "active_customers",
      columns: ["customer_id bigint", "org_id bigint", "last_order_at ts"],
      kind: "v",
      dependsOn: ["customers", "orders"],
    },
  ],
};

const GROUP_STYLE: Record<string, { name: string; color: string }> = {
  identity: { name: "Identity", color: "blue" },
  catalog: { name: "Catalog", color: "amber" },
  orders: { name: "Orders", color: "green" },
  billing: { name: "Billing", color: "violet" },
  support: { name: "Support", color: "pink" },
  analytics: { name: "Analytics", color: "teal" },
};

/** A 46-table SaaS commerce schema in `public`, with six groups. */
export function mediumFixture(): { schema: Schema; groups: Groups } {
  const enums = new Set(ACME_ENUMS.map((e) => e.name));
  const entities: Entity[] = [];
  const groups: Groups = {};
  for (const [id, specs] of Object.entries(ACME)) {
    for (const spec of specs) entities.push(buildEntity("public", spec, enums));
    groups[id] = { ...GROUP_STYLE[id]!, members: specs.map((s) => `public.${s.name}`) };
  }
  const months = ["2026_07", "2026_08", "2026_09", "2026_10"];
  const partitions: Partition[] = months.map((m, i) => ({
    parent_schema: "public",
    parent: "events",
    child_schema: "public",
    child: `events_${m}`,
    bound: `FOR VALUES FROM ('${m.replace("_", "-")}-01') TO ('${months[i + 1]?.replace("_", "-") ?? "2026-11"}-01')`,
  }));
  return { schema: schemaOf("acme", entities, ACME_ENUMS, partitions), groups };
}

// ---- Large: a seeded generator ----------------------------------------------

/** mulberry32: small, fast and the same everywhere. */
function random(seed: number) {
  let a = seed >>> 0;
  return () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

const DOMAINS: Record<string, string[]> = {
  identity: [
    "organizations",
    "users",
    "teams",
    "roles",
    "permissions",
    "sessions",
    "devices",
    "api_keys",
    "sso_connections",
    "domains",
  ],
  catalog: [
    "products",
    "variants",
    "categories",
    "brands",
    "collections",
    "media",
    "attributes",
    "price_lists",
    "bundles",
    "reviews",
  ],
  sales: [
    "customers",
    "orders",
    "quotes",
    "carts",
    "discounts",
    "channels",
    "regions",
    "sales_reps",
    "leads",
    "opportunities",
  ],
  billing: [
    "accounts",
    "invoices",
    "payments",
    "subscriptions",
    "plans",
    "credits",
    "tax_ids",
    "ledgers",
    "payouts",
    "disputes",
  ],
  logistics: [
    "warehouses",
    "shipments",
    "carriers",
    "parcels",
    "routes",
    "drivers",
    "pickups",
    "returns",
    "stock_moves",
    "bins",
  ],
  support: [
    "tickets",
    "conversations",
    "macros",
    "queues",
    "agents",
    "slas",
    "surveys",
    "articles",
    "escalations",
    "channels_inbound",
  ],
};

const SUFFIXES = [
  "events",
  "notes",
  "history",
  "attachments",
  "settings",
  "snapshots",
  "assignments",
  "comments",
  "metrics",
  "exports",
  "imports",
  "webhooks",
  "versions",
  "limits",
  "flags",
];

const EXTRA: string[] = [
  "name text!",
  "description text",
  "status text! ='active'",
  "code varchar! unique",
  "amount numeric",
  "quantity int! =0",
  "position smallint",
  "metadata json! ='{}'",
  "enabled bool! =true",
  "external_id uuid",
  "starts_on date",
  "ends_on date",
  "tags text[]",
  "updated_at ts! now",
  "archived_at ts",
];

function singular(table: string): string {
  return table.endsWith("ies")
    ? `${table.slice(0, -3)}y`
    : table.endsWith("ses")
      ? table.slice(0, -2)
      : table.replace(/s$/, "");
}

/** `count` tables over six schemas, references within and across them. Same seed, same schema. */
export function largeFixture(count = 300, seed = 7): Schema {
  const rand = random(seed);
  const pick = <T>(list: readonly T[]) => list[Math.floor(rand() * list.length)]!;
  const schemas = Object.keys(DOMAINS);
  const perSchema = Math.ceil(count / schemas.length);
  const entities: Entity[] = [];
  const partitions: Partition[] = [];
  const enums = new Set<string>();

  for (const schema of schemas) {
    const bases = DOMAINS[schema]!;
    const specs: TableSpec[] = [];
    bases.forEach((base, i) => {
      const columns = ["id pk"];
      if (schema !== "identity" || i > 0) columns.push(`org_id -> identity.organizations`);
      // References to earlier base tables of the schema, and now and then across.
      for (let r = 0; r < 2; r++) {
        if (i > 1 && rand() < 0.55) {
          const target = bases[Math.floor(rand() * i)]!;
          const col = `${singular(target)}_id`;
          if (!columns.some((c) => c.startsWith(`${col} `)))
            columns.push(`${col} ${rand() < 0.3 ? "?->" : "->"} ${target}`);
        }
      }
      if (schema !== "identity" && rand() < 0.35) columns.push(`owner_id ?-> identity.users`);
      const extras = 3 + Math.floor(rand() * 7);
      for (let e = 0; e < extras; e++) {
        const c = pick(EXTRA);
        if (!columns.some((x) => x.split(" ")[0] === c.split(" ")[0])) columns.push(c);
      }
      columns.push("created_at ts! now");
      specs.push({
        name: base,
        columns,
        comment: rand() < 0.3 ? `The ${schema} ${singular(base).replace(/_/g, " ")}` : undefined,
      });
    });
    let k = 0;
    while (specs.length < perSchema - 1) {
      const base = bases[k % bases.length]!;
      const suffix = SUFFIXES[Math.floor(k / bases.length) % SUFFIXES.length]!;
      k++;
      const name = `${singular(base)}_${suffix}`;
      if (specs.some((s) => s.name === name)) continue;
      const columns = ["id pk", `${singular(base)}_id -> ${base}`];
      if (rand() < 0.25) columns.push(`author_id ?-> identity.users`);
      const extras = 2 + Math.floor(rand() * 5);
      for (let e = 0; e < extras; e++) {
        const c = pick(EXTRA);
        if (!columns.some((x) => x.split(" ")[0] === c.split(" ")[0])) columns.push(c);
      }
      columns.push("created_at ts! now");
      const partitioned = suffix === "events" && rand() < 0.5;
      specs.push({
        name,
        columns,
        ...(partitioned
          ? {
              kind: "p" as const,
              partitionKey: "RANGE (created_at)",
              primaryKey: ["id", "created_at"],
            }
          : {}),
      });
      if (partitioned) {
        for (const q of ["2026q1", "2026q2", "2026q3", "2026q4"]) {
          partitions.push({
            parent_schema: schema,
            parent: name,
            child_schema: schema,
            child: `${name}_${q}`,
            bound: `FOR VALUES FROM ('${q}') TO ('${q}+')`,
          });
        }
      }
    }
    // A couple of views per schema, reading two tables.
    specs.push({
      name: `${schema}_overview`,
      columns: ["id bigint", "name text", "total numeric"],
      kind: "v",
      dependsOn: [bases[0]!, bases[1]!],
    });
    for (const spec of specs) entities.push(buildEntity(schema, spec, enums));
  }
  return schemaOf("warehouse", entities.slice(0, count), [], partitions);
}

// ---- SQL, the way remus-sql lays it out ------------------------------------

const quote = (s: string) => `'${s.replace(/'/g, "''")}'`;

/** Close enough to remus-sql for the fixtures: types, tables, partitions, views, FKs, indexes, comments. */
export function renderFixtureSql(schema: Schema): string {
  const out: string[] = [
    `-- Schema DDL for ${schema.database} (dev fixture, rendered in the browser).`,
    "",
  ];
  const schemas = [...new Set((schema.entities ?? []).map((e) => e.schema))].filter(
    (s) => s !== "public",
  );
  for (const s of schemas) out.push(`CREATE SCHEMA IF NOT EXISTS ${s};`);
  if (schemas.length) out.push("");
  for (const e of schema.enums ?? []) {
    out.push(
      `CREATE TYPE ${e.schema}.${e.name} AS ENUM (${(e.labels ?? []).map(quote).join(", ")});`,
    );
  }
  if (schema.enums?.length) out.push("");
  const tables = (schema.entities ?? []).filter((e) => e.kind === "r" || e.kind === "p");
  for (const t of tables) {
    const lines = (t.columns ?? []).map((c) => {
      let def = `    ${c.name} ${c.type_kind === "e" ? `${c.type_schema}.${c.type}` : c.type}`;
      if (c.identity) def += " GENERATED ALWAYS AS IDENTITY";
      if (c.generated) def += ` GENERATED ALWAYS AS (${c.default}) STORED`;
      else if (c.default) def += ` DEFAULT ${c.default}`;
      if (c.not_null) def += " NOT NULL";
      return def;
    });
    for (const c of t.constraints ?? []) {
      if (c.kind !== "f") lines.push(`    CONSTRAINT ${c.name} ${c.definition}`);
    }
    out.push(
      `CREATE TABLE ${t.schema}.${t.name} (\n${lines.join(",\n")}\n)${t.partition_key ? ` PARTITION BY ${t.partition_key}` : ""};`,
      "",
    );
  }
  for (const p of schema.partitions ?? []) {
    out.push(
      `CREATE TABLE ${p.child_schema}.${p.child} PARTITION OF ${p.parent_schema}.${p.parent} ${p.bound ?? "DEFAULT"};`,
    );
  }
  if (schema.partitions?.length) out.push("");
  for (const v of (schema.entities ?? []).filter((e) => e.kind === "v" || e.kind === "m")) {
    const kind = v.kind === "m" ? "MATERIALIZED VIEW" : "VIEW";
    out.push(
      `CREATE ${kind} ${v.schema}.${v.name} AS\n${v.view_definition?.trim() ?? "SELECT 1;"}`,
      "",
    );
  }
  for (const t of tables) {
    for (const c of (t.constraints ?? []).filter((c) => c.kind === "f")) {
      out.push(
        `ALTER TABLE ONLY ${t.schema}.${t.name}\n    ADD CONSTRAINT ${c.name} ${c.definition};`,
      );
    }
  }
  out.push("");
  for (const t of tables) for (const i of t.indexes ?? []) out.push(`${i.definition};`);
  out.push("");
  for (const t of schema.entities ?? []) {
    if (t.comment) out.push(`COMMENT ON TABLE ${t.schema}.${t.name} IS ${quote(t.comment)};`);
    for (const c of t.columns ?? []) {
      if (c.comment)
        out.push(`COMMENT ON COLUMN ${t.schema}.${t.name}.${c.name} IS ${quote(c.comment)};`);
    }
  }
  return `${out.join("\n").trim()}\n`;
}
