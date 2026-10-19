import { useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import {
  ClockIcon,
  ClockRotateRightIcon,
  EyeClosedIcon,
  EyeIcon,
  GitCompareIcon,
  UploadIcon,
} from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Spinner } from "@/components/ui/spinner";
import { Tooltip } from "@/components/ui/tooltip";
import { databaseQuery } from "@/features/databases/queries";
import { useOpenUpload } from "@/features/databases/upload-context";
import { canManageMembers } from "@/features/orgs/roles";
import { orgQuery } from "@/features/orgs/queries";
import type { VersionLink } from "@/features/time-machine/components/history-panel";
import { TimeMachine } from "@/features/time-machine/components/time-machine";
import { TimeMachineSkeleton } from "@/features/time-machine/components/time-machine-skeleton";
import { type History, parseHistory } from "@/features/time-machine/history";
import { timelineQuery } from "@/features/time-machine/queries";
import type { TimeMachineState } from "@/features/time-machine/state";
import { preloadAutoLayout } from "@/features/schema/graph/use-auto-layout";
import { SchemaParseError } from "@/features/schema/model";
import { useDebounced } from "@/lib/use-debounced";

// The time machine, at /app/<org>/<db>/time-machine: the database's versions
// superposed on one graph, a timeline under it. Shareable with the range and
// the cursor (?from=&to=&at=, version numbers), the mode (?mode=cursor), the
// filters (?only=changes, ?group=, ?q=) and the selected table (?table=);
// admins can bring removed versions in (?removed=1).
interface Search {
  from?: number;
  to?: number;
  at?: number;
  mode?: "cursor";
  only?: "changes";
  group?: string;
  q?: string;
  table?: string;
  removed?: 1;
}

const number = (v: unknown) => {
  const n = typeof v === "number" ? v : typeof v === "string" ? Number(v) : NaN;
  return Number.isInteger(n) && n > 0 ? n : undefined;
};
const text = (v: unknown) => (typeof v === "string" && v ? v : undefined);

export const Route = createFileRoute("/$org/$db/time-machine")({
  staticData: { fullBleed: true },
  validateSearch: (search: Record<string, unknown>): Search => ({
    from: number(search.from),
    to: number(search.to),
    at: number(search.at),
    mode: search.mode === "cursor" ? "cursor" : undefined,
    only: search.only === "changes" ? "changes" : undefined,
    group: text(search.group),
    q: text(search.q),
    table: text(search.table),
    removed: search.removed === 1 || search.removed === "1" ? 1 : undefined,
  }),
  loaderDeps: ({ search }) => ({ removed: search.removed === 1 }),
  loader: async ({ context: { queryClient }, params: { org, db }, deps }) => {
    const [database, orgData] = await Promise.all([
      queryClient.ensureQueryData(databaseQuery(org, db)),
      queryClient.ensureQueryData(orgQuery(org)),
    ]);
    // Removed versions are an admin's: anyone else would only get a 403.
    const removed = deps.removed && canManageMembers(orgData.role);
    if (database.versions > 1) {
      void queryClient.prefetchQuery(timelineQuery(org, database, removed));
      // The first visit lays out every table of the history: the layout
      // engine loads while the history does.
      preloadAutoLayout();
    }
  },
  head: () => ({ meta: [{ title: "Time machine – romulus" }] }),
  component: TimeMachinePage,
});

/**
 * Version numbers from the address, as indexes into the history (or the
 * defaults). A version not in it (removed, the removed ones left out) is the
 * nearest one before it, as when the history changes under an open view.
 */
function initialState(history: History, search: Search): TimeMachineState {
  const index = (n: number | undefined) => {
    if (n === undefined) return -1;
    const exact = history.versions.findIndex((v) => v.number === n);
    return exact >= 0 ? exact : history.versions.findLastIndex((v) => v.number < n);
  };
  const last = history.versions.length - 1;
  let from = index(search.from);
  let to = index(search.to);
  if (from < 0) from = 0;
  if (to < 0) to = last;
  if (to <= from) [from, to] = [0, last];
  let at = index(search.at);
  if (at < from || at > to) at = to;
  return {
    range: { from, to },
    at,
    mode: search.mode === "cursor" ? "cursor" : "superposed",
    changedOnly: search.only === "changes",
    group: search.group && history.groups[search.group] ? search.group : null,
    query: search.q ?? "",
    table: search.table && history.tables.some((t) => t.id === search.table) ? search.table : null,
  };
}

function TimeMachinePage() {
  const { org, db } = Route.useParams();
  const search = Route.useSearch();
  const navigate = useNavigate({ from: Route.fullPath });
  const { data: database } = useSuspenseQuery(databaseQuery(org, db));
  const { data: orgData } = useSuspenseQuery(orgQuery(org));
  const admin = canManageMembers(orgData.role);
  const removed = admin && search.removed === 1;
  const openUpload = useOpenUpload();
  const timeline = useQuery({
    ...timelineQuery(org, database, removed),
    enabled: database.versions > 1,
    // The history shown stays while the next one loads (removed versions
    // in or out, a version pushed meanwhile), never another database's.
    placeholderData: (previous, query) =>
      query?.queryKey[1] === database.id ? previous : undefined,
  });

  const history = React.useMemo(() => {
    if (!timeline.data) return undefined;
    try {
      return parseHistory(timeline.data);
    } catch (error) {
      return { error: error instanceof SchemaParseError ? error.message : String(error) };
    }
  }, [timeline.data]);

  // The address follows the view once it pauses (a played range would
  // otherwise write a history entry per version).
  // The state's indexes are into the history it came with: a call made
  // before the history changed (removed versions in or out) writes that
  // history's version numbers, never the next one's.
  const toAddress = useDebounced((state: TimeMachineState, shown: History) => {
    const v = (i: number) => shown.versions[i]?.number;
    const last = shown.versions.length - 1;
    void navigate({
      search: {
        from: state.range.from === 0 ? undefined : v(state.range.from),
        to: state.range.to === last ? undefined : v(state.range.to),
        at: state.at === state.range.to ? undefined : v(state.at),
        mode: state.mode === "cursor" ? "cursor" : undefined,
        only: state.changedOnly ? "changes" : undefined,
        group: state.group ?? undefined,
        q: state.query.trim() || undefined,
        table: state.table ?? undefined,
        removed: removed ? 1 : undefined,
      },
      replace: true,
      resetScroll: false,
    });
  }, 300);

  const VersionDiff = React.useMemo<VersionLink>(() => {
    const versions = history && !("error" in history) ? history.versions : [];
    return function VersionDiff({ at, className, children, label }) {
      const v = versions[at];
      const before = versions[at - 1];
      if (!v || v.removed_at || before?.removed_at) {
        return (
          <span className={className} title={v?.removed_at ? "A removed version" : undefined}>
            {children}
          </span>
        );
      }
      return before ? (
        <Link
          to="/$org/$db/diff/$range"
          params={{ org, db, range: { from: before.number, to: v.number } }}
          aria-label={label}
          className={className}
        >
          {children}
        </Link>
      ) : (
        <Link
          to="/$org/$db/v/$number"
          params={{ org, db, number: v.number }}
          aria-label={label}
          className={className}
        >
          {children}
        </Link>
      );
    };
  }, [history, org, db]);

  if (database.versions === 0) {
    return (
      <Centered>
        <EmptyState
          icon={<ClockRotateRightIcon />}
          title="No versions to travel through yet"
          description="The time machine shows how the schema changed from version to version. Upload SQL, push from CI or import migrations to start the history."
          action={
            openUpload ? (
              <Button variant="primary" onClick={openUpload}>
                <UploadIcon />
                Upload SQL
              </Button>
            ) : undefined
          }
        />
      </Centered>
    );
  }
  if (database.versions === 1 && database.latest) {
    return (
      <Centered>
        <EmptyState
          icon={<ClockRotateRightIcon />}
          title="Only one version so far"
          description={`The time machine superposes versions to show how the schema changed. v${database.latest.number} is the only one: the next upload, push, import or published draft gives it something to compare.`}
          action={
            <div className="flex flex-wrap justify-center gap-2">
              <Button asChild>
                <Link to="/$org/$db" params={{ org, db }}>
                  Open v{database.latest.number}
                </Link>
              </Button>
              {openUpload && (
                <Button variant="primary" onClick={openUpload}>
                  <UploadIcon />
                  Upload SQL
                </Button>
              )}
            </div>
          }
        />
      </Centered>
    );
  }
  if (timeline.error) {
    return (
      <div className="p-4 md:p-6">
        <Alert
          variant="destructive"
          title="Could not load the history"
          action={
            <Button size="sm" loading={timeline.isFetching} onClick={() => void timeline.refetch()}>
              Retry
            </Button>
          }
        >
          {timeline.error instanceof ApiError && timeline.error.status === 503
            ? "It took too long to compute. What was computed is kept: retrying is quicker."
            : timeline.error.message}
        </Alert>
      </div>
    );
  }
  if (!history) return <TimeMachineSkeleton />;
  if ("error" in history) {
    return (
      <div className="p-4 md:p-6">
        <Alert variant="destructive" title="This history cannot be drawn">
          A version is not one this viewer understands ({history.error}).
        </Alert>
      </div>
    );
  }
  if (history.versions.length < 2) {
    // Removed versions leave fewer than the count said.
    return (
      <Centered>
        <EmptyState
          icon={<ClockRotateRightIcon />}
          title="Only one version left"
          description="The others were removed. The time machine needs two versions or more."
          action={
            <Button asChild>
              <Link to="/$org/$db/history" params={{ org, db }}>
                <ClockIcon />
                See the history
              </Link>
            </Button>
          }
        />
      </Centered>
    );
  }

  return (
    <TimeMachine
      // Another set of versions (removed ones in or out) is another view.
      // Another database is another view; the same one with removed versions
      // in or out, or a version pushed meanwhile, keeps its place (by number).
      key={database.id}
      layoutKey={database.id}
      history={history}
      initial={initialState(history, search)}
      onStateChange={toAddress}
      VersionLink={VersionDiff}
      actions={(state) => {
        const from = history.versions[state.range.from]!;
        const to = history.versions[state.range.to]!;
        const comparable = !from.removed_at && !to.removed_at;
        return (
          <>
            {timeline.isPlaceholderData && <Spinner label="Loading the history" />}
            {admin && (
              <Tooltip
                content={removed ? "Leave removed versions out" : "Bring removed versions in"}
                side="bottom"
              >
                <Button
                  variant={removed ? "secondary" : "ghost"}
                  size="icon-sm"
                  aria-pressed={removed}
                  aria-label="Show removed versions"
                  onClick={() =>
                    void navigate({
                      search: (s) => ({ ...s, removed: removed ? undefined : 1 }),
                      replace: true,
                    })
                  }
                >
                  {removed ? <EyeIcon /> : <EyeClosedIcon />}
                </Button>
              </Tooltip>
            )}
            {comparable && (
              <Button size="sm" asChild className="max-sm:px-2">
                <Link
                  to="/$org/$db/diff/$range"
                  params={{ org, db, range: { from: from.number, to: to.number } }}
                  aria-label={`Compare v${from.number} → v${to.number}`}
                >
                  <GitCompareIcon />
                  <span className="max-sm:hidden">
                    Compare v{from.number} → v{to.number}
                  </span>
                </Link>
              </Button>
            )}
          </>
        );
      }}
    />
  );
}

function Centered({ children }: { children: React.ReactNode }) {
  return <div className="px-4 py-10 md:px-8">{children}</div>;
}
