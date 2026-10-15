import { cn } from "@/lib/utils";

// A still of the product for the sign-in page: the table graph, the SQL that
// defines it, and two collaborators at work. Purely decorative (the caller
// hides it from assistive tech), drawn from tokens so it follows the theme.

interface Column {
  name: string;
  type: string;
  key?: "pk" | "fk";
}

function TableNode({
  name,
  columns,
  className,
  selected = false,
}: {
  name: string;
  columns: Column[];
  className?: string;
  selected?: boolean;
}) {
  return (
    <div
      className={cn(
        "absolute w-52 overflow-hidden rounded-lg border bg-card shadow-raised",
        selected ? "border-presence-1 ring-2 ring-presence-1/20" : "border-border",
        className,
      )}
    >
      <div className="flex h-8 items-center gap-2 border-b border-border px-3">
        <span className="size-2 rounded-xs bg-primary" />
        <span className="font-mono text-xs font-semibold text-foreground">{name}</span>
      </div>
      <ul className="py-1">
        {columns.map((c) => (
          <li key={c.name} className="flex h-6 items-center gap-2 px-3 font-mono text-xs">
            <span className="flex-1 text-foreground">{c.name}</span>
            {c.key && (
              <span
                className={cn(
                  "rounded-xs px-1 text-[0.625rem] font-medium",
                  c.key === "pk" ? "bg-primary/10 text-primary" : "bg-info/10 text-info",
                )}
              >
                {c.key.toUpperCase()}
              </span>
            )}
            <span className="text-faint-foreground">{c.type}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

const presence = {
  1: { text: "text-presence-1", bg: "bg-presence-1" },
  3: { text: "text-presence-3", bg: "bg-presence-3" },
} as const;

function Cursor({
  name,
  className,
  color,
}: {
  name: string;
  className?: string;
  color: keyof typeof presence;
}) {
  return (
    <div className={cn("absolute flex items-start", className)}>
      <svg viewBox="0 0 16 16" className={cn("size-4 drop-shadow-sm", presence[color].text)}>
        <path d="M1 1l5.5 13.5 1.9-5.6L14 7z" fill="currentColor" stroke="var(--color-card)" />
      </svg>
      <span
        className={cn(
          "mt-3 -ml-1 rounded-sm px-1.5 py-0.5 text-[0.6875rem] font-medium text-presence-foreground shadow-xs",
          presence[color].bg,
        )}
      >
        {name}
      </span>
    </div>
  );
}

const sql = [
  ["k", "CREATE TABLE "],
  ["i", "memberships"],
  ["p", " ("],
  ["n", "\n  org_id  "],
  ["t", "uuid"],
  ["k", " REFERENCES "],
  ["i", "organizations"],
  ["p", ","],
  ["n", "\n  user_id "],
  ["t", "uuid"],
  ["k", " REFERENCES "],
  ["i", "users"],
  ["p", ","],
  ["n", "\n  role    "],
  ["t", "member_role"],
  ["k", " NOT NULL"],
] as const;

const tone = {
  k: "text-primary",
  i: "text-foreground",
  t: "text-info",
  p: "text-muted-foreground",
  n: "text-foreground",
} as const;

export function SchemaPreview() {
  return (
    <div className="absolute inset-0">
      <div className="absolute inset-0 bg-[radial-gradient(var(--color-border-strong)_1px,transparent_1px)] [mask-image:radial-gradient(ellipse_at_center,black_35%,transparent_80%)] bg-size-[1.375rem_1.375rem]" />

      <div className="absolute top-1/2 left-1/2 h-[30rem] w-[36rem] -translate-x-1/2 -translate-y-1/2">
        <svg
          className="absolute inset-0 size-full overflow-visible text-border-strong"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.5"
        >
          <path d="M208 70 C 260 70, 250 178, 316 178" />
          <path d="M208 330 C 260 330, 250 202, 316 202" />
          <circle cx="316" cy="178" r="3" className="fill-card" />
          <circle cx="316" cy="202" r="3" className="fill-card" />
        </svg>

        <TableNode
          name="organizations"
          className="top-6 left-0"
          columns={[
            { name: "id", type: "uuid", key: "pk" },
            { name: "name", type: "text" },
            { name: "slug", type: "text" },
          ]}
        />
        <TableNode
          name="memberships"
          selected
          className="top-[8.5rem] left-[19.75rem]"
          columns={[
            { name: "org_id", type: "uuid", key: "fk" },
            { name: "user_id", type: "uuid", key: "fk" },
            { name: "role", type: "member_role" },
          ]}
        />
        <TableNode
          name="users"
          className="top-[16.75rem] left-0"
          columns={[
            { name: "id", type: "uuid", key: "pk" },
            { name: "email", type: "text" },
            { name: "name", type: "text" },
          ]}
        />

        <Cursor name="Alice" color={1} className="top-[9.25rem] left-[33.25rem]" />

        <div className="absolute top-[18.5rem] left-[14.5rem] w-[21.5rem] overflow-hidden rounded-lg border border-border bg-card shadow-popover">
          <div className="flex h-7 items-center gap-1.5 border-b border-border px-3">
            <span className="size-1.5 rounded-full bg-border-strong" />
            <span className="size-1.5 rounded-full bg-border-strong" />
            <span className="size-1.5 rounded-full bg-border-strong" />
            <span className="ml-2 font-mono text-[0.6875rem] text-faint-foreground">
              schema.sql
            </span>
          </div>
          <pre className="px-3 py-2.5 font-mono text-xs leading-5 whitespace-pre">
            {sql.map(([kind, text], i) => (
              <span key={i} className={tone[kind]}>
                {text}
              </span>
            ))}
            {/* Bob is typing at the end of the line: a caret and his name. */}
            <span className="relative mx-px inline-block h-4 w-0.5 translate-y-0.5 bg-presence-3" />
            <span className="ml-1 rounded-sm bg-presence-3 px-1 py-px font-sans text-[0.625rem] font-medium text-presence-foreground">
              Bob
            </span>
            <span className="text-muted-foreground">{"\n);"}</span>
          </pre>
        </div>
      </div>
    </div>
  );
}
