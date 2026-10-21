import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import * as React from "react";

import {
  DatabaseIcon,
  NavArrowDownIcon,
  OpenNewWindowIcon,
  PlayIcon,
  XmarkIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  Menu,
  MenuContent,
  MenuLabel,
  MenuRadioGroup,
  MenuRadioItem,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Sheet, SheetClose, SheetContent, SheetTitle } from "@/components/ui/sheet";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { showUpgrade } from "@/features/billing/upgrade";
import { Tooltip } from "@/components/ui/tooltip";
import { ApiError } from "@/api/client";
import type { components } from "@/api/schema.gen";
import { instanceQuery, meQuery } from "@/features/auth/queries";
import { databaseQuery } from "@/features/databases/queries";
import type { ShownMigration } from "@/features/migration/components/migration-view";
import { PLAYGROUND_MAJOR, playgroundsQuery } from "@/features/playground/queries";
import { timeAgo } from "@/lib/time";
import { cn } from "@/lib/utils";

import { useAcknowledgeTest, useCancelTest, useStartTest } from "../mutations";
import {
  type MigrationTest,
  type StartOptions,
  type TestSource,
  isLive,
  latestFor,
  testQuery,
  testsQuery,
} from "../queries";
import { type RowsChoice, choiceDetail, choiceKey, choiceLabel, rowsChoices } from "../rows";
import { TestReport, TestStatusBadge } from "./test-report";

type TestGate = components["schemas"]["TestGate"];

interface TestButtonProps {
  org: string;
  db: string;
  source: TestSource;
  /** Developers and admins run tests; reviewers read them. */
  canRun: boolean;
  /** Why it cannot run now (the SQL has errors…): the button is disabled and says so. */
  blocked?: string;
  /** The migration as the view shows it: what is tested (renames, concurrently). */
  shown?: Pick<ShownMigration, "renames" | "concurrently" | "pending" | "statements">;
  /**
   * The version the rows must hold (the migration's base): a playground at
   * it can lend a copy of its rows, the person's own by default.
   */
  base?: number | null;
  className?: string;
}

/**
 * "Test on data": one click runs the migration up and down on a playground
 * of its own and opens its report in a side panel. On a copy of the
 * person's playground at the base version when there is one (the menu next
 * to it picks another, or fresh rows), on fresh generated rows otherwise.
 * After it, the last test of this migration (of the text as it is, for a
 * draft), which opens the panel too.
 */
export function MigrationTestButton({
  org,
  db,
  source,
  canRun,
  blocked: blockedBy,
  shown,
  base,
  className,
}: TestButtonProps) {
  const { data: tests } = useQuery(testsQuery(org, db, source));
  const latest = latestFor(tests, source);
  const [open, setOpen] = React.useState(false);
  const [testId, setTestId] = React.useState<string | null>(null);
  const start = useStartTest(org, db, source);
  const { data: me } = useQuery(meQuery);
  const { data: instance } = useQuery(instanceQuery);
  const { data: database } = useQuery(databaseQuery(org, db));
  // Tests run on playgrounds: none on this server, nothing to run.
  const runs = canRun && instance?.playgrounds !== false;
  const major = database?.pg_major;
  const { data: playgrounds } = useQuery({
    ...playgroundsQuery(org, db),
    enabled: runs && base != null,
  });
  const choices = rowsChoices(
    playgrounds,
    base,
    me?.id,
    source.kind === "draft" ? source.draft : undefined,
  );
  const [picked, setPicked] = React.useState<string | null>(null);
  const choice: RowsChoice = choices.find((c) => choiceKey(c) === picked) ?? choices[0]!;
  const blocked =
    blockedBy ??
    (major !== undefined && major !== PLAYGROUND_MAJOR
      ? `Tests run on Postgres ${PLAYGROUND_MAJOR} for now, and this database targets Postgres ${major}.`
      : undefined) ??
    (shown?.statements === 0
      ? "Nothing to run: the migration is empty."
      : shown?.pending
        ? "The migration is updating: test it once it shows what you chose."
        : undefined);
  const run = (extra: StartOptions = {}) =>
    start.mutate(
      {
        renames: shown?.renames,
        concurrently: shown?.concurrently,
        playground: choice.kind === "playground" ? choice.playground.id : undefined,
        ...extra,
      },
      {
        onSuccess: (test) => {
          setTestId(test.id);
          setOpen(true);
        },
        onError: (error) => {
          if (showUpgrade(error)) return;
          toast.error("The test could not start", {
            description: error instanceof ApiError ? error.message : "Check your connection.",
          });
        },
      },
    );
  const describedBy = React.useId();
  // Not `disabled` when blocked: it stays focusable, and its tooltip says why.
  const runButton = (
    <Button
      size="sm"
      onClick={() => !blocked && run()}
      loading={start.isPending}
      aria-disabled={blocked ? true : undefined}
      aria-describedby={blocked ? describedBy : undefined}
      className={cn(
        choices.length > 1 && "rounded-r-none",
        blocked && "cursor-not-allowed opacity-45",
      )}
    >
      <PlayIcon aria-hidden />
      Test on data
    </Button>
  );
  return (
    <div className={cn("flex items-center gap-2", className)}>
      {runs && (
        <div role="group" aria-label="Test on data" className="inline-flex">
          <Tooltip
            content={
              blocked ??
              (choice.kind === "fresh"
                ? "On a playground of its own, with fresh generated rows"
                : choice.mine
                  ? "On a copy of your playground's rows. Yours is not changed."
                  : `On a copy of the rows of ${choiceLabel(choice)}. It is not changed.`)
            }
          >
            {runButton}
          </Tooltip>
          {choices.length > 1 && (
            <Menu>
              <MenuTrigger asChild>
                <Button
                  size="sm"
                  disabled={Boolean(blocked) || start.isPending}
                  aria-label={`Rows to test on: ${choiceLabel(choice)}`}
                  className="-ml-px rounded-l-none px-1.5"
                >
                  <NavArrowDownIcon />
                </Button>
              </MenuTrigger>
              <MenuContent align="end" className="w-72">
                <MenuLabel>Test on</MenuLabel>
                <MenuRadioGroup value={choiceKey(choice)} onValueChange={setPicked}>
                  {choices.map((c) => (
                    <MenuRadioItem
                      key={choiceKey(c)}
                      value={choiceKey(c)}
                      description={choiceDetail(c)}
                    >
                      {c.kind === "fresh" ? <PlayIcon /> : <DatabaseIcon />}
                      {choiceLabel(c)}
                    </MenuRadioItem>
                  ))}
                </MenuRadioGroup>
              </MenuContent>
            </Menu>
          )}
        </div>
      )}
      {blocked && (
        <span id={describedBy} className="sr-only">
          {blocked}
        </span>
      )}
      {latest && (
        <Button
          variant="ghost"
          size="sm"
          onClick={() => {
            setTestId(latest.id);
            setOpen(true);
          }}
          aria-label={`Open the last test (${latest.status})`}
        >
          <TestStatusBadge test={latest} />
          {!isLive(latest) && (
            <span className="text-muted-foreground max-sm:hidden">
              {timeAgo(latest.finished_at ?? latest.created_at)}
            </span>
          )}
        </Button>
      )}
      <MigrationTestSheet
        org={org}
        db={db}
        source={source}
        canRun={canRun}
        open={open}
        onOpenChange={setOpen}
        testId={testId}
        onShow={setTestId}
        onRun={run}
        running={start.isPending}
      />
    </div>
  );
}

interface SheetProps {
  org: string;
  db: string;
  source: TestSource;
  canRun: boolean;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  testId: string | null;
  onShow: (id: string) => void;
  /** Runs the migration again (with a generated down: `{ down: "generate" }`). */
  onRun: (options?: StartOptions) => void;
  running: boolean;
}

/** A test in a side panel, with the earlier tests of the same migration below. */
export function MigrationTestSheet({
  org,
  db,
  source,
  canRun,
  open,
  onOpenChange,
  testId,
  onShow,
  onRun,
  running,
}: SheetProps) {
  const { data: tests } = useQuery({ ...testsQuery(org, db, source), enabled: open });
  return (
    <Sheet open={open} onOpenChange={onOpenChange}>
      <SheetContent
        side="right"
        className="w-[44rem] max-w-full bg-background text-foreground"
        aria-describedby={undefined}
      >
        <div className="flex items-center justify-between gap-2 border-b border-border px-4 py-3">
          <SheetTitle className="text-base font-semibold">Migration test</SheetTitle>
          <div className="flex items-center gap-1">
            {testId && (
              <Tooltip content="Open as a page">
                <Button variant="ghost" size="icon-sm" asChild>
                  <Link
                    to="/$org/$db/tests/$id"
                    params={{ org, db, id: testId }}
                    aria-label="Open as a page"
                  >
                    <OpenNewWindowIcon />
                  </Link>
                </Button>
              </Tooltip>
            )}
            <SheetClose asChild>
              <Button variant="ghost" size="icon-sm" aria-label="Close">
                <XmarkIcon />
              </Button>
            </SheetClose>
          </div>
        </div>
        <div className="grid flex-1 content-start gap-6 overflow-y-auto px-4 py-4">
          {testId ? (
            <ShownTest
              org={org}
              db={db}
              id={testId}
              canRun={canRun}
              onRun={onRun}
              running={running}
            />
          ) : (
            <p className="text-base text-muted-foreground">No test yet.</p>
          )}
          <EarlierTests tests={tests} shown={testId} onShow={onShow} />
        </div>
      </SheetContent>
    </Sheet>
  );
}

export function ShownTest({
  org,
  db,
  id,
  canRun,
  onRun,
  running,
}: {
  org: string;
  db: string;
  id: string;
  canRun: boolean;
  onRun?: (options?: StartOptions) => void;
  running?: boolean;
}) {
  const { data: test, error } = useQuery(testQuery(org, db, id));
  const cancel = useCancelTest(org, db, id);
  const acknowledge = useAcknowledgeTest(org, db, id);
  if (error) {
    return (
      <p role="alert" className="text-base text-destructive">
        This test could not be loaded. {error instanceof ApiError ? error.message : ""}
      </p>
    );
  }
  if (!test) return <ReportSkeleton />;
  return (
    <TestReport
      test={test}
      canRun={canRun}
      onRunAgain={onRun && (() => onRun())}
      onGenerateDown={onRun && (() => onRun({ down: "generate" }))}
      runningAgain={running}
      onCancel={() =>
        cancel.mutate(undefined, {
          onSuccess: () => toast.success("Test stopped"),
          onError: (e) =>
            toast.error(e instanceof ApiError ? e.message : "It could not be stopped."),
        })
      }
      cancelling={cancel.isPending}
      onAcknowledge={() =>
        acknowledge.mutateAsync().then(
          () => toast.success("Data loss acknowledged"),
          (e: unknown) => {
            toast.error(e instanceof ApiError ? e.message : "It could not be acknowledged.");
            throw e;
          },
        )
      }
      href={`/app/${org}/${db}/tests/${id}`}
    />
  );
}

function ReportSkeleton() {
  return (
    <div className="grid gap-3" aria-hidden>
      <Skeleton className="h-5 w-24" />
      <Skeleton className="h-6 w-4/5" />
      <Skeleton className="h-4 w-3/5" />
      <Skeleton className="h-40 w-full" />
    </div>
  );
}

function EarlierTests({
  tests,
  shown,
  onShow,
}: {
  tests: MigrationTest[] | undefined;
  shown: string | null;
  onShow: (id: string) => void;
}) {
  const earlier = tests?.filter((t) => t.id !== shown) ?? [];
  if (earlier.length === 0) return null;
  return (
    <section aria-labelledby="earlier-tests" className="grid gap-2">
      <h3 id="earlier-tests" className="text-sm font-medium text-muted-foreground">
        Earlier tests of this migration
      </h3>
      <ul className="divide-y divide-border overflow-hidden rounded-lg border border-border bg-card">
        {earlier.map((test) => (
          <li key={test.id}>
            <Button
              variant="ghost"
              className="h-auto w-full justify-start gap-3 rounded-none px-3 py-2 text-left"
              onClick={() => onShow(test.id)}
            >
              <TestStatusBadge test={test} />
              <span className="min-w-0 flex-1 truncate font-normal text-foreground">
                {test.summary?.headline ?? test.failure ?? "Running…"}
              </span>
              <span className="shrink-0 font-normal text-muted-foreground">
                {test.created_by?.name ?? "Someone"}, {timeAgo(test.created_at)}
              </span>
            </Button>
          </li>
        ))}
      </ul>
    </section>
  );
}

/**
 * For the publish dialog: how the newest test of the draft's text as it is
 * went ("Tested on data 3 minutes ago, by Alice"), or that there is none;
 * with `gate` (the preview's `test_gate`), the test that lets it publish,
 * or why publishing waits.
 */
export function TestedLine({
  org,
  db,
  draft,
  gate,
}: {
  org: string;
  db: string;
  draft: string;
  gate?: TestGate;
}) {
  const { data: tests, isPending } = useQuery(
    testsQuery(org, db, { kind: "draft", draft, sha256: "" }),
  );
  if (isPending) return null;
  const required = !!gate?.required && !gate.nothing_to_test;
  const latest =
    tests?.find((t) => t.id === gate?.passing) ?? tests?.find((t) => t.current === true);
  const held = required && !gate?.passing ? gate?.reason : null;
  return (
    <p className="flex flex-wrap items-center gap-2 text-sm text-muted-foreground">
      {latest ? (
        <>
          <TestStatusBadge test={latest} />
          <span>
            {isLive(latest)
              ? "Testing this migration on data now."
              : `Tested on data ${timeAgo(latest.finished_at ?? latest.created_at)}, by ${latest.created_by?.name ?? "someone"}.`}
            {held && !isLive(latest) && ` ${held}`}
          </span>
        </>
      ) : (
        <>
          <PlayIcon aria-hidden className="size-4 shrink-0" />
          <span>
            {held ??
              (required
                ? "Publishing needs a passing test on data first: Test on data is in the Migration tab."
                : "Not tested on data yet: Test on data is in the Migration tab.")}
          </span>
        </>
      )}
    </p>
  );
}
