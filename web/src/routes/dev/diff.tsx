import { createFileRoute, Link, notFound } from "@tanstack/react-router";

import { DatabaseIcon, GitCompareIcon } from "@/components/icons";
import { AppShell } from "@/components/shell/app-shell";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { SidebarItem, SidebarNav, SidebarSection } from "@/components/ui/sidebar";
import { toast } from "@/components/ui/toaster";
import type { VersionSummary } from "@/features/databases/queries";
import type { ChangeFilter, SchemaDiff } from "@/features/diff/changes";
import { CompareBar } from "@/features/diff/components/compare-bar";
import { DiffSkeleton } from "@/features/diff/components/diff-skeleton";
import { Identical } from "@/features/diff/components/identical";
import { SchemaDiffView } from "@/features/diff/components/schema-diff-view";
import type { Layout } from "@/features/schema/layout";
import { parseSchema } from "@/features/schema/model";
import { devRoutes } from "@/lib/dev-routes";
import { useDebounced } from "@/lib/use-debounced";

import { orgs, user } from "./-fixtures";

// The diff view on the blog fixture (v1 → v2), inside the app shell, for
// building and reviewing it without a stack. Dev builds only.
//   ?fixture=large                300 tables, a tenth of them changed (the perf spec's)
//   ?fixture=large-all            300 tables, every one changed
//   ?state=loading|identical      the other states
//   ?change=table:public.posts    the selected change
//   ?show=added|removed|changed   the filter

interface Search {
  fixture?: "large" | "large-all";
  state?: "loading" | "identical";
  change?: string;
  show?: ChangeFilter;
}

/** Where the blog's tables sat in v1 (as the viewer fixture has them). */
const LAYOUT: Layout = {
  "public.users": { x: 0, y: 120 },
  "public.profiles": { x: 0, y: 380 },
  "public.posts": { x: 340, y: 0 },
  "public.tags": { x: 340, y: 340 },
  "public.comments": { x: 700, y: 0 },
  "public.post_tags": { x: 700, y: 250 },
  "public.published_posts": { x: 700, y: 390 },
};

const VERSIONS: VersionSummary[] = [
  {
    id: "v2",
    number: 2,
    origin: "push",
    source_kind: "remus_json",
    message: "Reactions and subscriptions",
    created_at: new Date(Date.now() - 3_600_000).toISOString(),
    author: { kind: "token", id: "t1", name: "CI" },
  },
  {
    id: "v1",
    number: 1,
    origin: "upload",
    source_kind: "sql",
    message: "First cut",
    created_at: new Date(Date.now() - 86_400_000 * 3).toISOString(),
    author: { kind: "user", id: "u-alice", name: "Alice Martin" },
  },
];

export const Route = createFileRoute("/dev/diff")({
  validateSearch: (search: Record<string, unknown>): Search => ({
    fixture:
      search.fixture === "large" || search.fixture === "large-all" ? search.fixture : undefined,
    state: search.state === "loading" || search.state === "identical" ? search.state : undefined,
    change: typeof search.change === "string" ? search.change : undefined,
    show:
      search.show === "added" || search.show === "removed" || search.show === "changed"
        ? search.show
        : undefined,
  }),
  loaderDeps: ({ search }) => ({ fixture: search.fixture }),
  beforeLoad: () => {
    if (!devRoutes) throw notFound();
  },
  loader: async ({ deps }) => {
    if (deps.fixture) {
      const { largeDiffFixture } = await import("@/features/schema/fixtures/synthetic-diff");
      const every = deps.fixture === "large-all";
      return { ...largeDiffFixture(300, { every }), layout: undefined, large: true };
    }
    const [before, after, diff, sqlBefore, sqlAfter] = await Promise.all([
      import("@/features/schema/fixtures/blog.json"),
      import("@/features/schema/fixtures/blog-v2.json"),
      import("@/features/schema/fixtures/blog-diff.json"),
      import("@/features/schema/fixtures/blog.sql?raw"),
      import("@/features/schema/fixtures/blog-v2.sql?raw"),
    ]);
    return {
      before: parseSchema(before.default),
      after: parseSchema(after.default),
      diff: diff.default as unknown as SchemaDiff,
      sql: { before: sqlBefore.default, after: sqlAfter.default },
      layout: LAYOUT,
      large: false,
    };
  },
  pendingMs: 0,
  head: () => ({ meta: [{ title: "Diff view – romulus" }] }),
  component: DiffPreview,
});

function DiffPreview() {
  const data = Route.useLoaderData();
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  // As on the real page: the address follows once the reader pauses.
  const selectInUrl = useDebounced(
    (change: string | null) =>
      void navigate({ search: (s) => ({ ...s, change: change ?? undefined }), replace: true }),
    250,
  );
  const nav = (
    <SidebarNav aria-label="Organization">
      <SidebarSection>
        <SidebarItem asChild icon={<DatabaseIcon />} label="Schema viewer">
          <Link to="/dev/viewer" />
        </SidebarItem>
        <SidebarItem asChild icon={<GitCompareIcon />} label="Diff view">
          <Link to="/dev/diff" />
        </SidebarItem>
      </SidebarSection>
    </SidebarNav>
  );
  return (
    <AppShell
      user={user}
      org={orgs[0]!}
      orgs={orgs}
      nav={nav}
      breadcrumb={<span className="truncate font-mono text-sm text-foreground">diff view</span>}
      onSwitchOrg={(o) => toast(`Switched to ${o.name}`, { description: "Preview only." })}
      onCreateOrg={() => toast("Create organization", { description: "Preview only." })}
      onSignOut={() => toast("Signed out", { description: "Preview only." })}
    >
      <div className="flex h-full min-h-0 flex-col">
        <div className="flex min-h-14 shrink-0 items-center gap-3 border-b border-border px-4 py-2.5">
          <h1 className="text-lg font-semibold text-foreground">
            {data.large ? "Warehouse" : "Blog"}
          </h1>
          <Badge variant="outline">Dev fixture</Badge>
        </div>
        <CompareBar
          from={1}
          to={2}
          latest={2}
          versions={VERSIONS}
          onCompare={(from, to) =>
            toast(`Compare v${from} → v${to}`, { description: "Preview only." })
          }
        />
        <div className="relative min-h-0 flex-1">
          {search.state === "loading" ? (
            <DiffSkeleton />
          ) : search.state === "identical" ? (
            <Identical
              from={2}
              to={3}
              onCompare={(from, to) => toast(`Compare v${from} → v${to}`)}
              open={<Button>Open v3</Button>}
            />
          ) : (
            <SchemaDiffView
              before={data.before}
              after={data.after}
              diff={data.diff}
              sql={data.sql}
              key={search.fixture ?? "blog"}
              layout={data.layout}
              labels={{ before: "v1", after: "v2" }}
              defaultSelected={search.change}
              onSelectedChange={selectInUrl}
              filter={search.show ?? "all"}
              onFilterChange={(show) =>
                void navigate({
                  search: (s) => ({ ...s, show: show === "all" ? undefined : show }),
                  replace: true,
                })
              }
              storageKey="romulus:dev-diff"
            />
          )}
        </div>
      </div>
    </AppShell>
  );
}
