// Sample data for the dev-only shell preview. Not product data: real routes
// read the API through TanStack Query.
import type { ShellOrg, ShellUser } from "@/components/shell/types";

export const user: ShellUser = { id: "u-alice", name: "Alice Martin", email: "alice@romulus.test" };

export const orgs: ShellOrg[] = [
  { id: "o-acme", slug: "acme", name: "Acme", role: "admin" },
  { id: "o-northwind", slug: "northwind", name: "Northwind Traders", role: "developer" },
  { id: "o-lupidae", slug: "lupidae", name: "Lupidae Labs", role: "reviewer" },
];

export const people = [
  { id: "u-alice", name: "Alice Martin", email: "alice@acme.dev", role: "admin" },
  { id: "u-bob", name: "Bob Durand", email: "bob@acme.dev", role: "developer" },
  { id: "u-chloe", name: "Chloé Bernard", email: "chloe@acme.dev", role: "reviewer" },
  { id: "u-dmitri", name: "Dmitri Volkov", email: "dmitri@acme.dev", role: "developer" },
  { id: "u-emma", name: "Emma Leroy", email: "emma@acme.dev", role: "reviewer" },
  { id: "u-farid", name: "Farid Haddad", email: "farid@acme.dev", role: "developer" },
] as const;

export const databases = [
  {
    name: "billing",
    description: "Invoices, subscriptions and payment events",
    version: 14,
    updated: "2 hours ago",
    editing: people.slice(0, 3),
  },
  {
    name: "identity",
    description: "Users, sessions and organizations",
    version: 9,
    updated: "yesterday",
    editing: people.slice(3, 4),
  },
  {
    name: "warehouse",
    description: "Reporting schema fed by the nightly export",
    version: 31,
    updated: "Oct 2",
    editing: [],
  },
];
