import { useInfiniteQuery, useQuery, useSuspenseQuery } from "@tanstack/react-query";
import { createFileRoute, Link, notFound, useParams } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { ClockIcon, GitCompareIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { databaseQuery, versionsQuery } from "@/features/databases/queries";
import type { ChangeFilter } from "@/features/diff/changes";
import { CompareBar } from "@/features/diff/components/compare-bar";
import { DiffSkeleton } from "@/features/diff/components/diff-skeleton";
import { Identical } from "@/features/diff/components/identical";
import { SchemaDiffView } from "@/features/diff/components/schema-diff-view";
import { diffQuery, parseRange, rangeParam } from "@/features/diff/queries";
import { parseSchema, SchemaParseError } from "@/features/schema/model";
import { useDebounced } from "@/lib/use-debounced";

// Two versions compared, at /app/<org>/<db>/diff/<from>...<to>, shareable
// with what is selected (?change=) and the filter (?show=).
interface Search {
  change?: string;
  show?: ChangeFilter;
}

const FILTERS = new Set<string>(["added", "removed", "changed"]);

export const Route = createFileRoute("/$org/$db/diff/$range")({
  params: {
    parse: ({ range }) => {
      const parsed = parseRange(range);
      if (!parsed) throw notFound();
      return { range: parsed };
    },
    stringify: ({ range }) => ({ range: rangeParam(range.from, range.to) }),
  },
  validateSearch: (search: Record<string, unknown>): Search => ({
    change: typeof search.change === "string" ? search.change : undefined,
    show:
      typeof search.show === "string" && FILTERS.has(search.show)
        ? (search.show as ChangeFilter)
        : undefined,
  }),
  loader: async ({ context: { queryClient }, params: { org, db, range } }) => {
    let database = await queryClient.ensureQueryData(databaseQuery(org, db));
    const highest = Math.max(range.from, range.to);
    if (highest > (database.latest?.number ?? 0)) {
      // Newer than what this page knew: maybe pushed since.
      database = await queryClient.fetchQuery({ ...databaseQuery(org, db), staleTime: 0 });
    }
    if (highest <= (database.latest?.number ?? 0)) {
      void queryClient.prefetchQuery(diffQuery(org, db, database.id, range.from, range.to));
    }
  },
  head: ({ params }) => ({
    meta: [{ title: `v${params.range.from} → v${params.range.to} – romulus` }],
  }),
  notFoundComponent: NoSuchComparison,
  component: Compare,
});

function Compare() {
  const { org, db, range } = Route.useParams();
  const { from, to } = range;
  const search = Route.useSearch();
  const navigate = Route.useNavigate();
  const { data: database } = useSuspenseQuery(databaseQuery(org, db));
  const latest = database.latest?.number ?? 0;
  const [pickerOpened, setPickerOpened] = React.useState(false);
  const versions = useInfiniteQuery({ ...versionsQuery(org, db), enabled: pickerOpened });
  const missing = Math.max(from, to) > latest;
  const diff = useQuery({ ...diffQuery(org, db, database.id, from, to), enabled: !missing });

  // The address follows the selection, once the reader pauses: j held down
  // would otherwise match the routes again on every step.
  const selectInUrl = useDebounced(
    (change: string | null) =>
      void navigate({
        search: (s) => ({ ...s, change: change ?? undefined }),
        replace: true,
        resetScroll: false,
      }),
    250,
  );

  const compare = (f: number, t: number) =>
    void navigate({
      to: "/$org/$db/diff/$range",
      params: { org, db, range: { from: f, to: t } },
      // What was selected stays selected in the next pair, if it is there.
      search: (s) => s,
    });

  const models = React.useMemo(() => {
    if (!diff.data) return undefined;
    try {
      return {
        before: parseSchema(diff.data.models.before),
        after: parseSchema(diff.data.models.after),
      };
    } catch (error) {
      return { error: error instanceof SchemaParseError ? error.message : String(error) };
    }
  }, [diff.data]);

  const newest = Math.max(from, to);
  const open = (
    <Button size="sm" asChild>
      <Link
        to={newest === latest ? "/$org/$db" : "/$org/$db/v/$number"}
        params={{ org, db, number: newest }}
      >
        Open v{newest}
      </Link>
    </Button>
  );

  let body: React.ReactNode;
  if (missing) {
    body = <NoSuchComparison />;
  } else if (diff.error) {
    body =
      diff.error instanceof ApiError && diff.error.status === 404 ? (
        <NoSuchComparison />
      ) : (
        <div className="p-4 md:p-6">
          <Alert
            variant="destructive"
            title={`Could not compare v${from} and v${to}`}
            action={
              <Button size="sm" loading={diff.isFetching} onClick={() => void diff.refetch()}>
                Retry
              </Button>
            }
          >
            {diff.error.message}
          </Alert>
        </div>
      );
  } else if (models && "error" in models) {
    body = (
      <div className="p-4 md:p-6">
        <Alert variant="destructive" title="These versions cannot be drawn">
          A model is not one this viewer understands ({models.error}).
        </Alert>
      </div>
    );
  } else if (!diff.data || !models) {
    body = <DiffSkeleton />;
  } else if (diff.data.diff.summary.counts.length === 0) {
    body = (
      <Identical
        from={from}
        to={to}
        onCompare={compare}
        open={
          <Button asChild>
            <Link
              to={newest === latest ? "/$org/$db" : "/$org/$db/v/$number"}
              params={{ org, db, number: newest }}
            >
              Open v{newest}
            </Link>
          </Button>
        }
      />
    );
  } else {
    body = (
      <SchemaDiffView
        // A new pair is a new view: its own selection, graph and scroll.
        key={rangeParam(from, to)}
        before={models.before}
        after={models.after}
        diff={diff.data.diff}
        sql={diff.data.sql}
        layout={diff.data.layout.layout}
        groups={diff.data.layout.groups}
        labels={{ before: `v${from}`, after: `v${to}` }}
        defaultSelected={search.change}
        onSelectedChange={selectInUrl}
        filter={search.show ?? "all"}
        onFilterChange={(show) =>
          void navigate({
            search: (s) => ({ ...s, show: show === "all" ? undefined : show }),
            replace: true,
            resetScroll: false,
          })
        }
        storageKey="romulus:diff-view"
      />
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col">
      <CompareBar
        from={from}
        to={to}
        latest={latest}
        versions={versions.data?.pages.flatMap((p) => p.versions)}
        onOpenPicker={() => setPickerOpened(true)}
        onCompare={compare}
        allVersions={
          <Link to="/$org/$db/history" params={{ org, db }}>
            <ClockIcon />
            All {latest} versions
          </Link>
        }
        actions={!missing && open}
      />
      <div className="relative min-h-0 flex-1">{body}</div>
    </div>
  );
}

function NoSuchComparison() {
  // Not Route.useParams: this also renders when the range did not parse.
  const { org = "", db = "" } = useParams({ strict: false });
  return (
    <div className="px-4 py-10 md:px-8">
      <EmptyState
        icon={<GitCompareIcon />}
        title="Nothing to compare here"
        description="A comparison names two versions of this database, like /diff/3...4. One of these does not exist (yet)."
        action={
          <Button asChild>
            <Link to="/$org/$db/history" params={{ org, db }}>
              See every version
            </Link>
          </Button>
        }
      />
    </div>
  );
}
