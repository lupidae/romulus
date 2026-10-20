import type { Extension } from "@codemirror/state";
import * as React from "react";

import { ApiError } from "@/api/client";
import {
  CheckIcon,
  CopyIcon,
  DownloadIcon,
  EditPencilIcon,
  RefreshDoubleIcon,
  SearchIcon,
  WarningTriangleIcon,
  WifiOffIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { MigrationView, type RenameControl } from "@/features/migration/components/migration-view";
import { MigrationTestButton } from "@/features/migration-test/components/test-sheet";
import { type MigrationChoice, draftMigrationQuery } from "@/features/migration/queries";
import { save } from "@/features/migration/zip";
import { CodeView } from "@/features/schema/text/code-view";
import type { Editor } from "@/features/schema/text/codemirror";
import { modKey } from "@/lib/platform";
import { cn } from "@/lib/utils";

import type { Diagnostic, Draft } from "../queries";
import type { CompileView } from "../session";

/** A line under the tab bar saying what the tab is: the pane's own words, always visible. */
export function PaneNotice({
  icon,
  children,
  action,
  className,
}: {
  icon: React.ReactNode;
  children: React.ReactNode;
  action?: React.ReactNode;
  className?: string;
}) {
  return (
    <div
      className={cn(
        "flex min-h-9 shrink-0 items-center gap-2 border-b border-border bg-muted/60 py-1 pr-1.5 pl-3 text-sm text-muted-foreground [&>svg]:size-3.5 [&>svg]:shrink-0",
        className,
      )}
    >
      {icon}
      <p className="min-w-0 flex-1 py-0.5">{children}</p>
      {action}
    </div>
  );
}

/** The latest value that was not null: what to keep showing while the next one comes. */
function useLastDefined<T>(value: T | null): T | null {
  const [last, setLast] = React.useState<T | null>(value);
  if (value !== null && value !== last) setLast(value);
  return value ?? last;
}

/**
 * The Migration tab in schema mode: the migration romulus generates from the
 * draft's base to its SQL, live (each compile of the SQL brings a new one),
 * read-only, with the tools' layouts, the hazards marked, Copy and Download.
 * Editors can take it over: "Edit migration".
 */
export function GeneratedMigration({
  org,
  db,
  draft,
  compile,
  active,
  canEdit,
  onEdit,
  onJump,
  onUpdate,
  updating,
  renames,
}: {
  org: string;
  db: string;
  draft: Draft;
  compile: CompileView;
  /** The draft's rename decisions: everyone's, switched by editors. */
  renames?: RenameControl;
  /** The tab is shown: nothing is fetched while it is not. */
  active: boolean;
  /** May edit now (a developer or admin, the draft open). */
  canEdit: boolean;
  onEdit: () => void;
  onJump: (diagnostic: Diagnostic) => void;
  onUpdate: () => void;
  updating: boolean;
}) {
  const sha = useLastDefined(compile.sha256);
  const errors = compile.diagnostics.filter((d) => d.severity === "error");
  const base = draft.base_version;

  let blocked: React.ReactNode = null;
  if (draft.stale) {
    blocked = (
      <EmptyState
        icon={<RefreshDoubleIcon />}
        title={`v${draft.latest_version} is out`}
        description={`This migration would run on v${base}. Update the draft to see the one that runs on v${draft.latest_version}.`}
        action={
          canEdit && (
            <Button size="sm" loading={updating} onClick={onUpdate}>
              Update draft
            </Button>
          )
        }
      />
    );
  } else if (compile.state === "failed") {
    blocked = (
      <EmptyState
        icon={<WarningTriangleIcon />}
        title="The SQL has errors"
        description="The migration appears again once the SQL compiles."
        action={
          errors[0] && (
            <Button size="sm" onClick={() => onJump(errors[0]!)}>
              Show the first error
            </Button>
          )
        }
      />
    );
  } else if (compile.state === "unavailable") {
    blocked = (
      <EmptyState
        icon={<WifiOffIcon />}
        title="The SQL cannot be checked right now"
        description="The migration appears once the compiler is back. Your edits are kept."
      />
    );
  } else if (!sha) {
    blocked = (
      <div aria-busy aria-label="Preparing the migration" className="grid gap-2.5 px-4 py-3">
        {[70, 52, 84, 40, 76].map((w, i) => (
          <Skeleton key={i} className="h-3" style={{ width: `${w}%` }} />
        ))}
      </div>
    );
  }

  const query = (choice: MigrationChoice) => ({
    ...draftMigrationQuery(org, db, draft.id, sha ?? "", choice),
    // Someone typed between the compile and the request: a newer compile
    // (and key) is coming; try again meanwhile rather than flash an error.
    retry: (count: number, error: Error) =>
      error instanceof ApiError && error.code === "draft_changed" && count < 3,
    retryDelay: 400,
  });

  return (
    <div className="flex h-full min-h-0 flex-col">
      <PaneNotice
        icon={<RefreshDoubleIcon aria-hidden />}
        action={
          canEdit && (
            <Tooltip content="Write the migration yourself: the SQL and the graph then follow from it">
              <Button
                size="sm"
                variant="ghost"
                onClick={onEdit}
                disabled={compile.state === "failed"}
                className="shrink-0"
              >
                <EditPencilIcon />
                Edit migration
              </Button>
            </Tooltip>
          )
        }
      >
        {base
          ? `Generated from the SQL: what to run on v${base} to get this draft.`
          : "Generated from the SQL: it creates everything in this draft."}
      </PaneNotice>
      {active ? (
        <MigrationView
          query={query}
          primary={false}
          blocked={blocked}
          busy={compile.stale || compile.sha256 !== sha}
          renames={renames}
          actions={(shown) =>
            sha && (
              <MigrationTestButton
                org={org}
                db={db}
                source={{ kind: "draft", draft: draft.id, sha256: sha }}
                canRun={canEdit}
                shown={shown}
                base={base}
              />
            )
          }
          className="min-h-0 flex-1"
        />
      ) : (
        <div className="flex-1" />
      )}
    </div>
  );
}

/**
 * The Migration tab in migration mode: the migration written here, the
 * draft's source, edited together (everyone's carets, one undo history per
 * person, the compile's errors where they are).
 */
export function WrittenMigration({
  text,
  extensions,
  active,
  canEdit,
  baseVersion,
  onEditor,
  test,
}: {
  /** The migration now (seeds the editor; the binding owns it after). */
  text: string;
  /** Its Yjs binding (`draftExtensions` over the doc's `migration`). */
  extensions: Extension[] | undefined;
  active: boolean;
  canEdit: boolean;
  baseVersion: number | null | undefined;
  onEditor: (editor: Editor | null) => void;
  /** Test on data: the draft as compiled (`sha256`), or why not now. */
  test?: { org: string; db: string; draft: string; sha256: string | null; blocked?: string };
}) {
  return (
    <div className="flex h-full min-h-0 flex-col">
      <PaneNotice
        icon={<EditPencilIcon aria-hidden />}
        action={
          test && (
            <MigrationTestButton
              org={test.org}
              db={test.db}
              source={{ kind: "draft", draft: test.draft, sha256: test.sha256 ?? "" }}
              canRun={canEdit}
              blocked={
                test.blocked ??
                (test.sha256 ? undefined : "The migration is compiling: test it in a moment.")
              }
              base={baseVersion}
              className="shrink-0"
            />
          )
        }
      >
        {baseVersion ? `Written by hand from v${baseVersion}` : "Written by hand"}: the schema SQL
        and the graph follow from it.
        {!canEdit && " Developers and admins edit it."}
      </PaneNotice>
      <CodeView
        doc={text}
        language="sql"
        label="Migration"
        readOnly={!canEdit}
        extensions={extensions}
        active={active}
        onEditor={onEditor}
        className="min-h-0 flex-1"
      />
    </div>
  );
}

/** The tab bar's actions for the written migration: search, copy, download. */
export function WrittenMigrationActions({
  text,
  fileName,
  editor,
}: {
  text: string;
  fileName: string;
  editor: React.RefObject<Editor | null>;
}) {
  const [copied, setCopied] = React.useState(false);
  React.useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 1500);
    return () => window.clearTimeout(timer);
  }, [copied]);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      toast.success("Migration copied");
    } catch {
      toast.error("Could not copy", {
        description: "The browser blocked access to the clipboard.",
      });
    }
  };
  return (
    <>
      <Tooltip content="Search" shortcut={[modKey, "F"]}>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Search the migration"
          onClick={() => editor.current?.openSearch()}
        >
          <SearchIcon />
        </Button>
      </Tooltip>
      <Tooltip content="Copy the migration">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Copy the migration"
          disabled={text.trim() === ""}
          onClick={() => void copy()}
        >
          {copied ? <CheckIcon className="text-success" /> : <CopyIcon />}
        </Button>
      </Tooltip>
      <Tooltip content={`Download ${fileName}`}>
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label={`Download the migration as ${fileName}`}
          disabled={text.trim() === ""}
          onClick={() => save(fileName, text)}
        >
          <DownloadIcon />
        </Button>
      </Tooltip>
    </>
  );
}
