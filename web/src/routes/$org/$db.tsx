import { useSuspenseQuery } from "@tanstack/react-query";
import {
  createFileRoute,
  Link,
  notFound,
  Outlet,
  useMatchRoute,
  useNavigate,
} from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import {
  ClockIcon,
  DatabaseIcon,
  ImportIcon,
  NetworkRightIcon,
  SettingsIcon,
  UploadIcon,
} from "@/components/icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { TabNav, TabNavItem } from "@/components/ui/tab-nav";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { DatabaseMajorBadge } from "@/features/databases/components/postgres";
import { UploadDialog } from "@/features/databases/components/upload-dialog";
import { NewDraftButton, OpenDrafts } from "@/features/draft/components/drafts-section";
import { databaseQuery } from "@/features/databases/queries";
import { UploadContext } from "@/features/databases/upload-context";
import { orgQuery } from "@/features/orgs/queries";
import { canWrite } from "@/features/orgs/roles";
import { useShortcut } from "@/lib/use-shortcut";

// A database: its header and tabs (schema, history, settings), the upload
// dialog, and the tab's page below, edge to edge so the viewer gets the room.
export const Route = createFileRoute("/$org/$db")({
  staticData: { fullBleed: true },
  loader: async ({ context: { queryClient }, params }) => {
    try {
      const database = await queryClient.ensureQueryData(databaseQuery(params.org, params.db));
      return { name: database.name };
    } catch (error) {
      if (error instanceof ApiError && error.status === 404) throw notFound();
      throw error;
    }
  },
  head: ({ loaderData }) => ({ meta: [{ title: `${loaderData?.name ?? "Database"} – romulus` }] }),
  notFoundComponent: DatabaseNotFound,
  component: DatabaseLayout,
});

function DatabaseLayout() {
  const { org, db } = Route.useParams();
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  // Polled: versions pushed by CI show up without a reload.
  const { data: database } = useSuspenseQuery({
    ...databaseQuery(org, db),
    refetchInterval: 30_000,
  });
  const navigate = useNavigate();
  const matchRoute = useMatchRoute();
  const writer = canWrite(orgData.role);
  const [uploading, setUploading] = React.useState(false);
  const [draft, setDraft] = React.useState("");
  const openUpload = React.useCallback(() => setUploading(true), []);
  useShortcut("u", openUpload, writer);
  useShortcut("i", () => void navigate({ to: "/$org/$db/import", params: { org, db } }), writer);

  const params = { org, db };
  const onVersion = !!matchRoute({ to: "/$org/$db/v/$number", params, fuzzy: true });
  // A comparison and the time machine are read from the history, an import
  // adds to it: its tab stays current.
  const onDiff =
    !!matchRoute({ to: "/$org/$db/diff", params, fuzzy: true }) ||
    !!matchRoute({ to: "/$org/$db/import", params }) ||
    !!matchRoute({ to: "/$org/$db/time-machine", params });
  const count = database.versions;

  return (
    <UploadContext.Provider value={writer ? openUpload : null}>
      <div className="flex h-full min-h-0 flex-col">
        <header className="shrink-0 border-b border-border px-4 pt-4 md:px-6 md:pt-5">
          <div className="flex items-start justify-between gap-x-6 gap-y-3">
            <div className="flex min-w-0 items-center gap-3">
              <span className="flex size-9 shrink-0 items-center justify-center rounded-md border border-border bg-muted text-muted-foreground">
                <DatabaseIcon className="size-4" />
              </span>
              <div className="grid min-w-0">
                <h1 className="flex items-center gap-2 text-xl font-semibold text-foreground">
                  <span className="truncate">{database.name}</span>
                  <DatabaseMajorBadge org={org} database={database} editable={writer} />
                  {!writer && <Badge variant="outline">Read-only</Badge>}
                </h1>
                <p className="truncate text-sm text-muted-foreground">
                  {database.description ||
                    (count > 0
                      ? `${count === 1 ? "1 version" : `${count} versions`}`
                      : "No versions yet")}
                </p>
              </div>
            </div>
            <div className="flex shrink-0 items-center gap-2">
              <OpenDrafts org={org} db={db} />
              {writer && <NewDraftButton org={org} db={db} compact />}
              {writer && (
                <Tooltip content="Import migration files as versions" shortcut={["I"]}>
                  <Button asChild aria-label="Import migrations" className="max-sm:px-2.5">
                    <Link to="/$org/$db/import" params={params}>
                      <ImportIcon />
                      <span className="max-sm:hidden">Import</span>
                    </Link>
                  </Button>
                </Tooltip>
              )}
              {writer && (
                <Tooltip content="Upload SQL as a new version" shortcut={["U"]}>
                  <Button
                    variant="primary"
                    onClick={openUpload}
                    aria-label="Upload SQL"
                    className="max-sm:px-2.5"
                  >
                    <UploadIcon />
                    <span className="max-sm:hidden">Upload SQL</span>
                  </Button>
                </Tooltip>
              )}
            </div>
          </div>
          <TabNav aria-label={database.name} className="mt-3">
            <TabNavItem asChild>
              <Link
                to="/$org/$db"
                params={params}
                activeOptions={{ exact: true }}
                inactiveProps={onVersion ? { "aria-current": "page" } : undefined}
              >
                <NetworkRightIcon />
                Schema
              </Link>
            </TabNavItem>
            <TabNavItem asChild>
              <Link
                to="/$org/$db/history"
                params={params}
                inactiveProps={onDiff ? { "aria-current": "page" } : undefined}
              >
                <ClockIcon />
                History
                {count > 0 && (
                  <span className="rounded-sm bg-muted px-1 text-xs text-muted-foreground tabular-nums">
                    {count}
                  </span>
                )}
              </Link>
            </TabNavItem>
            {writer && (
              <TabNavItem asChild>
                <Link to="/$org/$db/settings" params={params}>
                  <SettingsIcon />
                  Settings
                </Link>
              </TabNavItem>
            )}
          </TabNav>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto">
          <Outlet />
        </div>
      </div>
      {writer && (
        <UploadDialog
          org={org}
          db={db}
          next={database.next_number}
          major={database.pg_major}
          open={uploading}
          onOpenChange={setUploading}
          draft={draft}
          onDraftChange={setDraft}
          onUploaded={(version) => {
            setUploading(false);
            toast.success(`v${version.number} created`, {
              description: version.message ?? `${database.name} is up to date.`,
            });
            void navigate({ to: "/$org/$db", params });
          }}
        />
      )}
    </UploadContext.Provider>
  );
}

function DatabaseNotFound() {
  const { org, db } = Route.useParams();
  return (
    <div className="px-4 py-10 md:px-8">
      <EmptyState
        icon={<DatabaseIcon />}
        title="No such database"
        description={`There is no database at /${org}/${db}. It may have been renamed or deleted.`}
        action={
          <Button asChild>
            <Link to="/$org" params={{ org }}>
              All databases
            </Link>
          </Button>
        }
      />
    </div>
  );
}
