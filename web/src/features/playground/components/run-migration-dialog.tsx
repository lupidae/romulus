import { useQuery } from "@tanstack/react-query";

import { ApiError, api, unwrap } from "@/api/client";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";

import { HAZARD } from "../format";
import type { Playground } from "../queries";

interface RunMigrationDialogProps {
  org: string;
  db: string;
  playground: Playground;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** Confirmed: run the migration of exactly this text (its `sha256`). */
  onRun: (sha256: string) => void;
}

/**
 * What a migration run will do, before it does it: the statements (from the
 * draft as it is now), their hazards, and that the rows are reset first. A
 * run is of exactly the text shown here.
 */
export function RunMigrationDialog({
  org,
  db,
  playground,
  open,
  onOpenChange,
  onRun,
}: RunMigrationDialogProps) {
  const draft = playground.draft;
  const preview = useQuery({
    queryKey: ["playgrounds", org, db, playground.id, "migration-preview", draft?.id],
    enabled: open && Boolean(draft),
    staleTime: 0,
    gcTime: 0,
    queryFn: async () => {
      const id = draft?.id ?? "";
      const previewed = await unwrap(
        api.GET("/api/orgs/{org}/databases/{db}/drafts/{id}/publish", {
          params: { path: { org, db, id } },
        }),
      );
      if (
        previewed.mode === "migration" ||
        previewed.diagnostics.some((d) => d.severity === "error")
      )
        return { previewed, statements: null };
      try {
        const migration = await unwrap(
          api.GET("/api/orgs/{org}/databases/{db}/drafts/{id}/migration", {
            params: { path: { org, db, id }, query: { sha256: previewed.sha256 } },
          }),
        );
        return { previewed, statements: migration.statements };
      } catch (error) {
        // A stale draft has no migration a database at the latest version
        // could run; the playground holds its base, so the run still can.
        if (error instanceof ApiError && error.code === "stale_base")
          return { previewed, statements: null };
        throw error;
      }
    },
  });
  const data = preview.data;
  const errors = data?.previewed.diagnostics.filter((d) => d.severity === "error") ?? [];
  const statements = data?.statements ?? null;
  const counts = {
    deletes: statements?.filter((s) => s.hazard === "deletes_data").length ?? 0,
    fails: statements?.filter((s) => s.hazard === "can_fail").length ?? 0,
  };
  const empty = statements !== null && statements.length === 0;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>Run the migration on these rows?</DialogTitle>
          <DialogDescription>
            The generated rows come back first, then the migration from v{playground.version} to “
            {draft?.name}” runs statement by statement, each measured. Everyone in the playground
            sees it, and it holds the migrated rows until someone resets them.
          </DialogDescription>
        </DialogHeader>
        {preview.isPending ? (
          <div className="grid gap-2">
            <Skeleton className="h-5 w-1/2" />
            <Skeleton className="h-24 w-full" />
          </div>
        ) : preview.isError ? (
          <Alert variant="destructive" title="Could not read the draft's migration">
            {preview.error instanceof ApiError ? preview.error.message : "Check your connection."}
          </Alert>
        ) : errors.length > 0 ? (
          <Alert variant="warning" title="The draft's SQL has errors">
            Fix them in the draft first: a migration runs only from SQL that compiles.
          </Alert>
        ) : empty ? (
          <Alert variant="info" title="Nothing to run">
            The draft changes nothing yet: there is no migration.
          </Alert>
        ) : statements ? (
          <div className="grid min-h-0 gap-2">
            <p className="flex flex-wrap items-center gap-2 text-sm text-foreground">
              {statements.length} statement{statements.length === 1 ? "" : "s"}
              {counts.deletes > 0 && (
                <Badge variant="destructive">{counts.deletes} delete data</Badge>
              )}
              {counts.fails > 0 && <Badge variant="warning">{counts.fails} can fail</Badge>}
            </p>
            <ol className="max-h-72 overflow-auto rounded-lg border border-border bg-muted/40 p-2 font-mono text-sm">
              {statements.map((s, i) => {
                const hazard = s.hazard ? HAZARD[s.hazard] : undefined;
                return (
                  <li key={i} className="flex items-start gap-2 py-0.5">
                    <span className="w-6 shrink-0 text-right text-faint-foreground tabular-nums">
                      {i + 1}
                    </span>
                    <span className="min-w-0 flex-1 whitespace-pre-wrap text-foreground">
                      {s.sql}
                    </span>
                    {hazard && (
                      <Badge variant={hazard.variant} className="shrink-0 font-sans">
                        {hazard.label}
                      </Badge>
                    )}
                  </li>
                );
              })}
            </ol>
          </div>
        ) : data?.previewed.mode === "migration" ? (
          <div className="grid gap-2">
            <p className="text-sm text-foreground">The migration as written in the draft:</p>
            <pre className="max-h-72 overflow-auto rounded-lg border border-border bg-muted/40 p-2 font-mono text-sm whitespace-pre-wrap text-foreground">
              {data.previewed.migration}
            </pre>
          </div>
        ) : (
          <p className="text-sm text-muted-foreground">
            The draft is behind the latest version; its migration from v{playground.version} still
            runs here.
          </p>
        )}
        <DialogFooter>
          <Button variant="ghost" onClick={() => onOpenChange(false)}>
            Cancel
          </Button>
          <Button
            variant="primary"
            disabled={!data || errors.length > 0 || empty}
            onClick={() => {
              if (data) onRun(data.previewed.sha256);
              onOpenChange(false);
            }}
          >
            Run migration
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}
