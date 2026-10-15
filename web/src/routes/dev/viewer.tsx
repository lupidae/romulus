import { createFileRoute, Link, notFound } from "@tanstack/react-router";
import { useState } from "react";

import { DatabaseIcon, GitCompareIcon, TableIcon } from "@/components/icons";
import { AppShell } from "@/components/shell/app-shell";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { SidebarItem, SidebarNav, SidebarSection } from "@/components/ui/sidebar";
import { toast } from "@/components/ui/toaster";
import {
  FIXTURE_IDS,
  FIXTURES,
  type FixtureData,
  type FixtureId,
  fixture,
} from "@/features/schema/fixtures";
import type { Layout } from "@/features/schema/layout";
import { SchemaViewer } from "@/features/schema/viewer/schema-viewer";
import { SchemaViewerSkeleton } from "@/features/schema/viewer/viewer-skeleton";

import { orgs, user } from "./-fixtures";

// The schema viewer on sample schemas, inside the app shell, for building and
// reviewing it before versions exist. Dev builds only.
//   ?fixture=small|medium|large|multi-schema|diff|empty   which schema
//   ?table=public.users                                   the selected table
//   ?state=loading                                        the loading state

interface Search {
  fixture?: FixtureId;
  table?: string;
  state?: "loading";
}

export const Route = createFileRoute("/dev/viewer")({
  validateSearch: (search: Record<string, unknown>): Search => ({
    fixture: FIXTURE_IDS.includes(search.fixture as FixtureId)
      ? (search.fixture as FixtureId)
      : undefined,
    table: typeof search.table === "string" ? search.table : undefined,
    state: search.state === "loading" ? "loading" : undefined,
  }),
  loaderDeps: ({ search }) => ({ fixture: search.fixture ?? "small" }),
  beforeLoad: () => {
    if (!import.meta.env.DEV) throw notFound();
  },
  loader: ({ deps }) => fixture(deps.fixture).load(),
  head: () => ({ meta: [{ title: "Schema viewer – romulus" }] }),
  pendingComponent: () => (
    <Frame>
      <SchemaViewerSkeleton />
    </Frame>
  ),
  component: ViewerPreview,
});

function Frame({ children, toolbar }: { children: React.ReactNode; toolbar?: React.ReactNode }) {
  const nav = (
    <SidebarNav aria-label="Organization">
      <SidebarSection>
        <SidebarItem asChild>
          <Link to="/dev/viewer" search={{ fixture: "small" }}>
            <DatabaseIcon />
            Databases
          </Link>
        </SidebarItem>
        <SidebarItem asChild>
          <Link to="/dev/viewer" search={{ fixture: "diff" }}>
            <GitCompareIcon />
            Diff example
          </Link>
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
      breadcrumb={<span className="truncate font-mono text-sm text-foreground">schema viewer</span>}
      onSwitchOrg={(o) => toast(`Switched to ${o.name}`, { description: "Preview only." })}
      onCreateOrg={() => toast("Create organization", { description: "Preview only." })}
      onSignOut={() => toast("Signed out", { description: "Preview only." })}
    >
      <div className="flex h-full min-h-0 flex-col">
        <div className="flex min-h-14 shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b border-border px-4 py-2.5">
          <span className="flex size-8 items-center justify-center rounded-md border border-border bg-muted text-muted-foreground">
            <TableIcon className="size-4" />
          </span>
          <h1 className="text-lg font-semibold text-foreground">Schema viewer</h1>
          <Badge variant="outline">Dev fixture</Badge>
          {toolbar}
        </div>
        <div className="min-h-0 flex-1">{children}</div>
      </div>
    </AppShell>
  );
}

function ViewerPreview() {
  const data = Route.useLoaderData();
  const search = Route.useSearch();
  const id = search.fixture ?? "small";
  const navigate = Route.useNavigate();
  const [editable, setEditable] = useState(false);

  const toolbar = (
    <>
      <p className="hidden min-w-0 flex-1 truncate text-sm text-muted-foreground lg:block">
        {fixture(id).description}
      </p>
      <div className="ml-auto flex items-center gap-2">
        <Button
          variant="secondary"
          size="sm"
          aria-pressed={editable}
          onClick={() => setEditable(!editable)}
          className={editable ? "border-primary/40 bg-primary/8 text-primary" : undefined}
        >
          {editable ? "Moving tables" : "Move tables"}
        </Button>
        <Select
          value={id}
          onValueChange={(v) => void navigate({ search: { fixture: v as FixtureId } })}
        >
          <SelectTrigger aria-label="Sample schema" className="w-44 sm:w-52">
            <SelectValue />
          </SelectTrigger>
          <SelectContent align="end">
            {FIXTURES.map((f) => (
              <SelectItem key={f.id} value={f.id}>
                {f.label}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>
    </>
  );

  return (
    <Frame toolbar={toolbar}>
      {search.state === "loading" ? (
        <SchemaViewerSkeleton />
      ) : (
        <Viewer
          key={id}
          data={data}
          editable={editable}
          selected={search.table ?? null}
          onSelectedChange={(table) =>
            void navigate({ search: (s) => ({ ...s, table: table ?? undefined }), replace: true })
          }
        />
      )}
    </Frame>
  );
}

/** One fixture's viewer; its layout state stands in for a draft's Yjs layout map. */
function Viewer({
  data,
  editable,
  selected,
  onSelectedChange,
}: {
  data: FixtureData;
  editable: boolean;
  selected: string | null;
  onSelectedChange: (table: string | null) => void;
}) {
  const [layout, setLayout] = useState<Layout | undefined>(data.layout);
  return (
    <SchemaViewer
      schema={data.schema}
      diff={data.diff}
      texts={data.texts}
      groups={data.groups}
      layout={layout}
      onLayoutChange={(positions) => setLayout((current) => ({ ...current, ...positions }))}
      editable={editable}
      selected={selected}
      onSelectedChange={onSelectedChange}
    />
  );
}
