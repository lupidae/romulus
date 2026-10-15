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
import { NetworkRightIcon, PageIcon } from "@/components/icons";
import { type ChangeStatus, changeOf, parseStatusMap } from "@/features/schema/diff";
import { SchemaGraph } from "@/features/schema/graph/schema-graph";
import type { Groups, Layout } from "@/features/schema/layout";
import { StatusBadge } from "@/features/schema/graph/status";
import { parseSchema } from "@/features/schema/model";
import { modKey } from "@/lib/platform";

import { usePublishDraft } from "../mutations";
import { type Diagnostic, draftKeys, type PublishPreview, publishPreviewQuery } from "../queries";

interface PublishDialogProps {
  org: string;
  db: string;
  draftId: string;
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
      <DialogContent className="top-[max(1rem,6vh)] flex max-h-[calc(100dvh-2rem)] max-w-3xl flex-col gap-4 sm:max-h-[88dvh]">
        {props.open && <PublishBody {...props} />}
      </DialogContent>
    </Dialog>
  );
}

function PublishBody({
  org,
  db,
  draftId,
  databaseName,
  layout,
  groups,
  next,
  onOpenChange,
  onJump,
  onPublished,
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
  const blocked = !data || data.stale || errors.length > 0 || !data.diff;

  const submit = () => {
    if (!data || blocked || publish.isPending) return;
    setNotice(null);
    publish.mutate(
      { sha256: data.sha256, message: message.trim() || undefined },
      {
        onSuccess: (version) => onPublished(version.number),
        onError: (error) => {
          if (!(error instanceof ApiError)) return setNotice("Could not publish. Try again.");
          if (["draft_changed", "validation", "stale_base"].includes(error.code)) {
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
          Creates v{number} of {databaseName} from this draft. The draft then closes, and everyone
          in it is told.
        </DialogDescription>
      </DialogHeader>

      {notice && <Alert variant="warning" title={notice} />}

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
                  Show in SQL
                </Button>
              </li>
            ))}
          </ul>
        </Alert>
      ) : (
        <Changes preview={data!} changes={changes} layout={layout} groups={groups} />
      )}

      <form
        onSubmit={(event) => {
          event.preventDefault();
          submit();
        }}
        className="grid gap-4"
      >
        <Field label="Message" description="Optional. What changed, in a line, for the history.">
          <Input
            value={message}
            onChange={(event) => setMessage(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) submit();
            }}
            placeholder="Add invoices and their line items"
            maxLength={1000}
            disabled={blocked}
          />
        </Field>
        <DialogFooter>
          <Button onClick={() => onOpenChange(false)}>Cancel</Button>
          <Button
            type="submit"
            variant="primary"
            disabled={blocked}
            loading={publish.isPending}
            aria-keyshortcuts="Meta+Enter Control+Enter"
            title={`${modKey} Enter`}
          >
            Publish v{number}
          </Button>
        </DialogFooter>
      </form>
    </>
  );
}

function Changes({
  preview,
  changes,
  layout,
  groups,
}: {
  preview: PublishPreview;
  changes: Change[];
  layout: Layout;
  groups: Groups;
}) {
  const { model: rawModel, base_model: rawBase, diff } = preview;
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
    <Tabs defaultValue="list" className="flex min-h-0 flex-1 flex-col gap-3">
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
        </TabsList>
      </div>
      <TabsContent
        value="list"
        className="mt-0 min-h-0 flex-1 overflow-y-auto rounded-lg border border-border"
      >
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
    </Tabs>
  );
}
