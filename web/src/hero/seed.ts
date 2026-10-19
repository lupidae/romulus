// What the hero opens on: a billing schema at v1, its two groups, where its
// tables sit, and a discussion already going on payments.

import type { MentionPerson } from "@/components/ui/mention-textarea";
import type { DocComment, DocThread } from "@/features/comments/model";
import type { Groups, Layout } from "@/features/schema/layout";

export const SEED_SQL = `CREATE TYPE invoice_status AS ENUM ('draft', 'open', 'paid', 'void');

CREATE TABLE customers (
  id         uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  email      text NOT NULL UNIQUE,
  name       text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE invoices (
  id          uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  customer_id uuid NOT NULL REFERENCES customers ON DELETE CASCADE,
  status      invoice_status NOT NULL DEFAULT 'draft',
  total_cents bigint NOT NULL DEFAULT 0 CHECK (total_cents >= 0),
  issued_at   timestamptz
);
CREATE INDEX invoices_customer_id_idx ON invoices (customer_id);
COMMENT ON COLUMN invoices.total_cents IS 'Sum of the lines, in cents';

CREATE TABLE invoice_lines (
  id          bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  invoice_id  uuid NOT NULL REFERENCES invoices ON DELETE CASCADE,
  description text NOT NULL,
  quantity    integer NOT NULL DEFAULT 1 CHECK (quantity > 0),
  unit_cents  bigint NOT NULL
);

CREATE TABLE payments (
  id         bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
  invoice_id uuid NOT NULL REFERENCES invoices,
  amount     numeric(12, 2) NOT NULL CHECK (amount > 0),
  paid_at    timestamptz NOT NULL DEFAULT now()
);
`;

/** Where the tables sit: side by side in a wide window… */
export const SEED_LAYOUT: Layout = {
  "public.customers": { x: 0, y: 60 },
  "public.invoices": { x: 320, y: 40 },
  "public.invoice_lines": { x: 640, y: -40 },
  "public.payments": { x: 640, y: 200 },
};

/** …and stacked on a phone, so they read at the zoom that fits them. */
export const SEED_LAYOUT_NARROW: Layout = {
  "public.customers": { x: 0, y: 0 },
  "public.invoices": { x: 0, y: 300 },
  "public.invoice_lines": { x: 280, y: 280 },
  "public.payments": { x: 280, y: 480 },
};

export const SEED_GROUPS: Groups = {
  people: { name: "People", color: "blue", members: ["public.customers"] },
  billing: {
    name: "Billing",
    color: "teal",
    members: ["public.invoices", "public.invoice_lines", "public.payments"],
  },
};

export const ME = { id: "visitor", name: "You" } as const;

export const PEOPLE: MentionPerson[] = [
  { id: "u-ada", name: "Ada Moreau", detail: "developer" },
  { id: "u-sam", name: "Sam Okafor", detail: "developer" },
  { id: "u-lin", name: "Lin Park", detail: "reviewer" },
];

const ago = (minutes: number) => new Date(Date.now() - minutes * 60_000).toISOString();

/** One open thread on payments, with a reply and a mention. */
export function seedDiscussion(): {
  threads: Record<string, DocThread>;
  comments: Record<string, DocComment>;
} {
  const lin = { id: "u-lin", name: "Lin Park" };
  const sam = { id: "u-sam", name: "Sam Okafor" };
  return {
    threads: {
      "seed-refunds": {
        anchor: { kind: "table", table: "public.payments" },
        by: lin.id,
        at: ago(95),
        resolved: null,
      },
    },
    comments: {
      "seed-refunds-1": {
        thread: "seed-refunds",
        author: lin,
        body: "Partial refunds: a refunds table, or an amount_refunded column here?",
        mentions: [],
        at: ago(95),
        edited: null,
      },
      "seed-refunds-2": {
        thread: "seed-refunds",
        author: sam,
        body: "A table, so each refund keeps its reason. @Lin Park I'll sketch it in the next draft.",
        mentions: [lin.id],
        at: ago(61),
        edited: null,
      },
    },
  };
}
