import { useQuery, useQueryClient } from "@tanstack/react-query";
import * as React from "react";

import { ApiError } from "@/api/client";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { EmptyState } from "@/components/ui/empty-state";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import {
  ChatLinesIcon,
  CheckIcon,
  CopyIcon,
  DatabaseScriptIcon,
  DownloadIcon,
  NetworkRightIcon,
  PageIcon,
} from "@/components/icons";
import { toast } from "@/components/ui/toaster";
import { PlanLimitNotice } from "@/features/billing/components/upgrade-prompt";
import { isPlanLimit } from "@/features/billing/plan-limit";
import { MigrationView, type RenameControl } from "@/features/migration/components/migration-view";
import { MigrationTestButton, TestedLine } from "@/features/migration-test/components/test-sheet";
import { type TestSource, testsQuery } from "@/features/migration-test/queries";
import { orgQuery } from "@/features/orgs/queries";
import { RenamesNote } from "@/features/migration/components/renames-note";
import { draftMigrationQuery } from "@/features/migration/queries";
import { save } from "@/features/migration/zip";
import { CodeView } from "@/features/schema/text/code-view";
import { type ChangeStatus, changeOf, parseStatusMap } from "@/features/schema/diff";
import { SchemaGraph } from "@/features/schema/graph/schema-graph";
import type { Groups, Layout } from "@/features/schema/layout";
import { StatusBadge } from "@/features/schema/graph/status";
import { parseSchema } from "@/features/schema/model";
import { modKey } from "@/lib/platform";

import { migrationFileName } from "../migration-file";
import { usePublishDraft } from "../mutations";
import { type Diagnostic, draftKeys, type PublishPreview, publishPreviewQuery } from "../queries";
import { useApprovalGate } from "../use-approval-gate";

interface PublishDialogProps {
  org: string;
  db: string;
  draftId: string;
  /** What a written migration downloads as. */
  draftName: string;
  databaseName: string;
  /** The draft's positions and groups: the diff is drawn where its tables are. */
  layout: Layout;
  groups: Groups;
  /** The version it would become, as the bar shows it. */
  next: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Close and show an error in the SQL. */
  onJump: (diagnostic: Diagnostic) => void;
  onPublished: (number: number) => void;
  /** Comment threads, open and resolved: kept with the version as its review. */
  threads?: { open: number; resolved: number };
  /** The draft's rename decisions: the migration shown, and what the version keeps. */
  renames?: RenameControl;
}

interface Change {
  key: string;
  kind: string;
  name: string;
  status: ChangeStatus;
  lines: string[];
}

function changesOf(preview: PublishPreview): Change[] {
  const diff = preview.diff;
  if (!diff) return [];
  const out: Change[] = [];
  const add = (
    kind: string,
    items: { schema?: string; name: string; status: string; lines: string[] }[],
  ) => {
    for (const item of items) {
      const status = changeOf(item.status as Parameters<typeof changeOf>[0]);
      if (!status) continue;
      const name = item.schema ? `${item.schema}.${item.name}` : item.name;
      out.push({ key: `${kind}:${name}`, kind, name, status, lines: item.lines });
    }
  };
  add("table", diff.entities);
  add("enum", diff.enums);
  add("domain", diff.domains);
  add("type", diff.composites);
  add("extension", diff.extensions);
  return out;
}

/**
 * "Publish v13": what the draft changes against its base, as a list and as
 * the highlighted graph, an optional message, and the one button. Publishing
 * stores exactly the text reviewed here; if someone typed meanwhile, the
 * dialog shows the new changes and asks again.
 */
export function PublishDialog(props: PublishDialogProps) {
  return (
    <Dialog open={props.open} onOpenChange={props.onOpenChange}>
      <DialogContent className="top-[max(1rem,6vh)] flex max-h-[calc(100dvh-2rem)] max-w-3xl flex-col gap-4 overflow-y-auto sm:max-h-[88dvh]">
        {props.open && <PublishBody {...props} />}
      </DialogContent>
    </Dialog>
  );
}

function PublishBody({
  org,
  db,
  draftId,
  draftName,
  databaseName,
  layout,
  groups,
  next,
  onOpenChange,
  onJump,
  onPublished,
  threads,
  renames,
}: PublishDialogProps) {
  const queryClient = useQueryClient();
  const preview = useQuery(publishPreviewQuery(org, db, draftId));
  const publish = usePublishDraft(org, db, draftId);
  const [message, setMessage] = React.useState("");
  const [notice, setNotice] = React.useState<string | null>(null);

  const data = preview.data;
  const number = data?.number ?? next;
  const errors = data?.diagnostics.filter((d) => d.severity === "error") ?? [];
  const changes = data ? changesOf(data) : [];
  const gate = useApprovalGate(data?.approvals);
  const tested = useTestGate(org, db, draftId, data);
  const unpublishable = !data || data.stale || errors.length > 0 || !data.diff;
  // A rename switched reaches the server through the draft's socket: until
  // the preview has it (and the migration shown is of it), what would be
  // published is not what is on screen.
  const onScreen = renames?.decisions;
  const previewBehind = !!data && !!onScreen && !sameDecisions(data.renames, onScreen);
  const [migrationPending, setMigrationPending] = React.useState(false);
  const catchingUp = previewBehind || migrationPending;
  const blocked = unpublishable || gate.blocked || tested.blocked || catchingUp;
  React.useEffect(() => {
    if (!previewBehind) return;
    const timer = window.setTimeout(
      () => void queryClient.invalidateQueries({ queryKey: draftKeys.preview(org, db, draftId) }),
      300,
    );
    return () => window.clearTimeout(timer);
  }, [previewBehind, data, queryClient, org, db, draftId]);

  const submit = () => {
    if (!data || blocked || publish.isPending) return;
    setNotice(null);
    publish.mutate(
      {
        sha256: data.sha256,
        message: message.trim() || undefined,
        override_reason: gate.overrideReason,
        test_override_reason: tested.overrideReason,
        renames: renames?.decisions,
      },
      {
        onSuccess: (version) => onPublished(version.number),
        onError: (error) => {
          if (!(error instanceof ApiError)) return setNotice("Could not publish. Try again.");
          // A cap of the plan (versions, schema size): said with the way out.
          if (isPlanLimit(error)) return setNotice(null);
          if (
            [
              "draft_changed",
              "renames_changed",
              "validation",
              "stale_base",
              "approvals_required",
              "passing_test_required",
              "tests_unavailable",
            ].includes(error.code)
          ) {
            // Show what is true now.
            void queryClient.invalidateQueries({ queryKey: draftKeys.preview(org, db, draftId) });
          }
          setNotice(
            error.code === "draft_changed"
              ? "Someone edited the draft while you were reviewing it. These are the changes now."
              : error.message,
          );
        },
      },
    );
  };

  return (
    <>
      <DialogHeader>
        <DialogTitle>Publish v{number}</DialogTitle>
        <DialogDescription>
          Creates v{number} of {databaseName} from this draft
          {data?.mode === "migration" ? ", with the migration it was written as" : ""}. The draft
          then closes, and everyone in it is told.
        </DialogDescription>
      </DialogHeader>

      {notice && <Alert variant="warning" title={notice} />}
      <PlanLimitNotice error={publish.error} org={org} />

      {preview.isPending ? (
        <div aria-busy className="grid gap-3" aria-label="Loading the changes">
          <Skeleton className="h-5 w-56" />
          <div className="grid gap-2 rounded-lg border border-border p-3">
            {[72, 56, 64].map((w) => (
              <Skeleton key={w} className="h-4" style={{ width: `${w}%` }} />
            ))}
          </div>
        </div>
      ) : preview.isError ? (
        <Alert
          variant="destructive"
          title="Could not prepare the changes"
          action={
            <Button size="sm" onClick={() => void preview.refetch()}>
              Retry
            </Button>
          }
        >
          {preview.error instanceof ApiError
            ? preview.error.message
            : "Check your connection and try again."}
        </Alert>
      ) : data!.stale ? (
        <Alert variant="warning" title={`v${data!.latest_version} was published meanwhile`}>
          This draft started from v{data!.base_version}. Updating it to v{data!.latest_version}{" "}
          comes with the next release of romulus; until then it cannot be published.
        </Alert>
      ) : errors.length > 0 ? (
        <Alert
          variant="destructive"
          title={`Fix ${errors.length === 1 ? "the error" : `the ${errors.length} errors`} before publishing`}
        >
          <ul className="mt-1 grid gap-1">
            {errors.slice(0, 5).map((d, i) => (
              <li key={i} className="flex items-start justify-between gap-3">
                <span className="text-foreground">{d.message}</span>
                <Button
                  size="sm"
                  variant="ghost"
                  onClick={() => {
                    onOpenChange(false);
                    onJump(d);
                  }}
                >
                  {data?.mode === "migration" ? "Show in the migration" : "Show in SQL"}
                </Button>
              </li>
            ))}
          </ul>
        </Alert>
      ) : (
        <Changes
          org={org}
          db={db}
          draftId={draftId}
          preview={data!}
          changes={changes}
          layout={layout}
          groups={groups}
          fileName={migrationFileName(draftName)}
          renames={renames}
          onMigrationPending={setMigrationPending}
          migration={(choice) =>
            draftMigrationQuery(org, db, draftId, data!.sha256, choice, () => {
              // The draft moved since the preview: show what is true now.
              void queryClient.invalidateQueries({ queryKey: draftKeys.preview(org, db, draftId) });
            })
          }
        />
      )}

      <form
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
        className="grid gap-4"
      >
        {data && !data.stale && errors.length === 0 && gate.element}
        {data && !data.stale && errors.length === 0 && changes.length > 0 && data.base_version && (
          <TestedLine org={org} db={db} draft={draftId} gate={data.test_gate} />
        )}
        {tested.element}
        {threads && threads.open + threads.resolved > 0 && (
          <p className="flex items-center gap-2 text-sm text-muted-foreground">
            <ChatLinesIcon aria-hidden className="size-4 shrink-0" />
            <span>
              {reviewLine(threads)} {threads.open + threads.resolved === 1 ? "stays" : "stay"} with
              v{number} as its review history, read-only. They never become database comments.
            </span>
          </p>
        )}
        <Field label="Message" description="Optional. What changed, in a line, for the history.">
          <Input
            value={message}
            onChange={(event) => setMessage(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) submit();
            }}
            placeholder="Add invoices and their line items"
            maxLength={1000}
            disabled={unpublishable}
          />
        </Field>
        <DialogFooter>
          <Button onClick={() => onOpenChange(false)}>Cancel</Button>
          <Button
            type="submit"
            variant="primary"
            disabled={blocked}
            loading={publish.isPending || (catchingUp && !unpublishable)}
            aria-keyshortcuts="Meta+Enter Control+Enter"
            title={`${modKey} Enter`}
          >
            {gate.overrideReason && tested.overrideReason
              ? `Publish v${number} without approvals or a test`
              : gate.overrideReason
                ? `Publish v${number} without approvals`
                : tested.overrideReason
                  ? `Publish v${number} without a test`
                  : `Publish v${number}`}
          </Button>
        </DialogFooter>
      </form>
    </>
  );
}

/**
 * An organization publishing only what passed a migration test: the
 * preview's `test_gate` (the API's `migration_tests::gate`, of exactly the
 * migration publishing would run). Held until such a test passed; where
 * tests cannot run, an admin may publish without one, saying why (the
 * audit log keeps it). The preview is read again whenever this draft's
 * tests change (one finishing, an acknowledgement).
 */
function useTestGate(org: string, db: string, draft: string, preview: PublishPreview | undefined) {
  const queryClient = useQueryClient();
  const { data: orgData } = useQuery(orgQuery(org));
  const [reason, setReason] = React.useState("");
  const testGate = preview?.test_gate;
  const held = !!testGate?.required && !testGate.nothing_to_test && !testGate.passing;
  const tests = useQuery({
    ...testsQuery(org, db, { kind: "draft", draft, sha256: "" }),
    enabled: held,
  });
  const seen = tests.data?.map((t) => `${t.id}:${t.status}`).join(",");
  const first = React.useRef(true);
  React.useEffect(() => {
    if (seen === undefined) return;
    if (first.current) {
      first.current = false;
      return;
    }
    void queryClient.invalidateQueries({ queryKey: draftKeys.preview(org, db, draft) });
  }, [seen, queryClient, org, db, draft]);
  const canOverride = held && testGate?.available === false && orgData?.role === "admin";
  const overrideReason = canOverride && reason.trim() ? reason.trim() : undefined;
  return {
    blocked: held && !overrideReason,
    overrideReason,
    element: canOverride ? (
      <Field
        label="Why publish without a test"
        description="Tests cannot run on this server, and the organization requires one. The audit log keeps your reason with the version."
      >
        <Input
          value={reason}
          onChange={(event) => setReason(event.target.value)}
          placeholder="Tested by hand on staging"
          maxLength={1000}
        />
      </Field>
    ) : null,
  };
}

/** Whether two sets of rename decisions say the same. */
function sameDecisions(a: Record<string, boolean>, b: Record<string, boolean>): boolean {
  const keys = Object.keys(a);
  return keys.length === Object.keys(b).length && keys.every((k) => a[k] === b[k]);
}

/** "3 comment threads (1 open)". */
function reviewLine({ open, resolved }: { open: number; resolved: number }) {
  const all = open + resolved;
  const threads = all === 1 ? "The comment thread" : `The ${all} comment threads`;
  return open === 0 || resolved === 0 ? threads : `${threads} (${open} open)`;
}

function Changes({
  org,
  db,
  draftId,
  preview,
  changes,
  layout,
  groups,
  fileName,
  migration,
  renames,
  onMigrationPending,
}: {
  org: string;
  db: string;
  draftId: string;
  preview: PublishPreview;
  changes: Change[];
  layout: Layout;
  groups: Groups;
  fileName: string;
  renames?: RenameControl;
  /** The migration tab is catching up with what was switched. */
  onMigrationPending: (pending: boolean) => void;
  /** The draft's migration, for a tool. */
  migration: React.ComponentProps<typeof MigrationView>["query"];
}) {
  const { model: rawModel, base_model: rawBase, diff } = preview;
  const source: TestSource = { kind: "draft", draft: draftId, sha256: preview.sha256 };
  const [tab, setTab] = React.useState("list");
  const graph = React.useMemo(() => {
    try {
      if (!rawModel || !diff) return null;
      const model = parseSchema(rawModel);
      const base = rawBase ? parseSchema(rawBase) : { ...model, entities: [] };
      return { model, diff: { base, status: parseStatusMap(diff.status) } };
    } catch {
      return null;
    }
  }, [rawModel, rawBase, diff]);

  if (changes.length === 0) {
    return (
      <div className="rounded-lg border border-border">
        <EmptyState
          className="py-8"
          title={
            preview.base_version
              ? `No schema changes since v${preview.base_version}`
              : "An empty schema"
          }
          description={`Publishing still records this draft's layout and groups as v${preview.number}.`}
        />
      </div>
    );
  }

  return (
    // Its own height (the panes are sized): with the approvals and a reason
    // below, the dialog scrolls rather than the panes overlapping them.
    <Tabs value={tab} onValueChange={setTab} className="flex shrink-0 flex-col gap-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="text-base font-medium text-foreground">
          {preview.diff!.summary.text[0]!.toUpperCase() + preview.diff!.summary.text.slice(1)}
          {preview.base_version ? (
            <span className="font-normal text-muted-foreground">
              {" "}
              since v{preview.base_version}
            </span>
          ) : null}
        </p>
        <TabsList aria-label="Show the changes as">
          <TabsTrigger value="list">
            <PageIcon />
            List
          </TabsTrigger>
          <TabsTrigger value="graph" disabled={!graph}>
            <NetworkRightIcon />
            Graph
          </TabsTrigger>
          <TabsTrigger value="migration">
            <DatabaseScriptIcon />
            Migration
          </TabsTrigger>
        </TabsList>
      </div>
      <TabsContent
        value="list"
        className="mt-0 max-h-[50dvh] overflow-y-auto rounded-lg border border-border"
      >
        <RenamesNote
          query={migration}
          decisions={renames?.decisions}
          onShow={() => setTab("migration")}
        />
        <ul className="divide-y divide-border">
          {changes.map((change) => (
            <li key={change.key} className="grid gap-1 px-3 py-2.5">
              <div className="flex min-w-0 items-center gap-2">
                <StatusBadge status={change.status} />
                <span className="truncate font-mono text-sm font-medium text-foreground">
                  {change.name}
                </span>
                <span className="text-xs text-muted-foreground">{change.kind}</span>
              </div>
              {change.status === "changed" && change.lines.length > 0 && (
                <ul className="grid gap-0.5 pl-1">
                  {change.lines.map((line, i) => (
                    <li key={i} className="text-sm text-muted-foreground">
                      {line.replaceAll("`", "")}
                    </li>
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>
      </TabsContent>
      <TabsContent
        value="graph"
        className="mt-0 h-[50dvh] min-h-64 overflow-hidden rounded-lg border border-border"
      >
        {graph && (
          <SchemaGraph
            schema={graph.model}
            diff={graph.diff}
            layout={layout}
            groups={groups}
            shortcuts={false}
          />
        )}
      </TabsContent>
      <TabsContent
        value="migration"
        className="mt-0 h-[50dvh] min-h-80 overflow-hidden rounded-lg border border-border"
      >
        {preview.mode === "migration" ? (
          <WrittenMigrationPreview
            text={preview.migration ?? ""}
            fileName={fileName}
            test={
              <MigrationTestButton
                org={org}
                db={db}
                source={source}
                canRun
                base={preview.base_version}
              />
            }
          />
        ) : (
          <MigrationView
            query={migration}
            primary={false}
            renames={renames}
            onPendingChange={onMigrationPending}
            actions={(shown) => (
              <MigrationTestButton
                org={org}
                db={db}
                source={source}
                canRun
                shown={shown}
                base={preview.base_version}
              />
            )}
          />
        )}
      </TabsContent>
    </Tabs>
  );
}

/** The migration a migration-mode draft was written as: what the version keeps. */
function WrittenMigrationPreview({
  text,
  fileName,
  test,
}: {
  text: string;
  fileName: string;
  /** Test on data. */
  test?: React.ReactNode;
}) {
  const [copied, setCopied] = React.useState(false);
  React.useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 2000);
    return () => window.clearTimeout(timer);
  }, [copied]);
  return (
    <div className="flex h-full min-h-0 flex-col bg-card">
      <div className="flex shrink-0 flex-wrap items-center gap-2 border-b border-border px-3 py-2">
        <p className="min-w-0 flex-1 text-sm text-muted-foreground">
          Written by hand in this draft. The version keeps it: download it again from its page.
        </p>
        {test}
        <Button
          size="sm"
          onClick={() => {
            void navigator.clipboard.writeText(text).then(
              () => setCopied(true),
              () => toast.error("Could not copy", { description: "Select the text and copy it." }),
            );
          }}
          aria-label={copied ? "Copied" : "Copy the migration"}
        >
          {copied ? <CheckIcon className="text-success" /> : <CopyIcon />}
          {copied ? "Copied" : "Copy"}
        </Button>
        <Button size="sm" onClick={() => save(fileName, text)}>
          <DownloadIcon />
          Download
        </Button>
      </div>
      <CodeView
        doc={text}
        language="sql"
        label="The migration, as written"
        readOnly
        active
        className="min-h-0 flex-1"
      />
    </div>
  );
}
