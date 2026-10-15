import * as React from "react";

import { ApiError } from "@/api/client";
import {
  CheckCircleIcon,
  PageIcon,
  WarningTriangleIcon,
  XmarkCircleIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { FileButton } from "@/components/ui/file-button";
import { Input } from "@/components/ui/input";
import { KbdGroup } from "@/components/ui/kbd";
import { Spinner } from "@/components/ui/spinner";
import { toast } from "@/components/ui/toaster";
import type { Editor } from "@/features/schema/text/codemirror";
import { CodeView } from "@/features/schema/text/code-view";
import { modKey } from "@/lib/platform";
import { cn } from "@/lib/utils";

import { useCompileSql, useUploadVersion } from "../mutations";
import type * as ExtensionsModule from "../sql-editor/extensions";
import {
  type CompileReport,
  type Diagnostic,
  diagnosticsOf,
  type VersionSummary,
} from "../queries";

/** The API's limit (the sandbox's `max_sql_bytes`). */
const MAX_BYTES = 1024 * 1024;
/** A pause in typing this long checks the text. */
const CHECK_AFTER_MS = 900;

type Extensions = typeof ExtensionsModule;

type Check =
  | { state: "idle" }
  | { state: "checking" }
  | { state: "done"; report: CompileReport }
  | { state: "unavailable"; message: string };

interface UploadDialogProps {
  org: string;
  db: string;
  /** The number the upload will get. */
  next: number;
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** The text to start from (kept between openings until uploaded). */
  draft: string;
  onDraftChange: (text: string) => void;
  onUploaded: (version: VersionSummary) => void;
}

/**
 * Paste or drop SQL, see whether it compiles (checked in the sandbox as you
 * pause), and create the next version. Errors are underlined in the text and
 * listed below it; a click on one puts the cursor there.
 */
export function UploadDialog(props: UploadDialogProps) {
  const { open, onOpenChange } = props;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent
        hideClose
        className={cn(
          "top-[4vh] flex h-[min(92dvh,50rem)] max-w-4xl flex-col gap-0 overflow-hidden p-0",
          "max-sm:top-0 max-sm:h-dvh max-sm:w-full max-sm:max-w-none max-sm:rounded-none max-sm:border-0",
        )}
      >
        {open && <UploadForm {...props} />}
      </DialogContent>
    </Dialog>
  );
}

function UploadForm({
  org,
  db,
  next,
  onOpenChange,
  draft,
  onDraftChange,
  onUploaded,
}: UploadDialogProps) {
  const upload = useUploadVersion(org, db);
  const compile = useCompileSql(org, db);
  const [loaded, setLoaded] = React.useState<{
    cm: Extensions;
    extensions: ReturnType<Extensions["uploadEditorExtensions"]>;
  } | null>(null);
  const cm = loaded?.cm;
  const [editor, setEditor] = React.useState<Editor | null>(null);
  const [text, setText] = React.useState(draft);
  const [message, setMessage] = React.useState("");
  const [check, setCheck] = React.useState<Check>({ state: "idle" });
  const [dragging, setDragging] = React.useState(false);
  const [fileName, setFileName] = React.useState<string | null>(null);
  const checkSeq = React.useRef(0);

  // What the editor's callbacks (created once, with the editor) call.
  const handlers = React.useRef({ submit: () => {}, change: (doc: string) => void doc });
  React.useEffect(() => {
    handlers.current.change = (doc) => {
      setText(doc);
      onDraftChange(doc);
    };
  }, [onDraftChange]);

  // CodeMirror and these extensions come with the editor's lazy chunk.
  React.useEffect(() => {
    let cancelled = false;
    void import("../sql-editor/extensions").then((module) => {
      if (cancelled) return;
      setLoaded({
        cm: module,
        extensions: module.uploadEditorExtensions({
          placeholder: "CREATE TABLE users (\n  id bigint PRIMARY KEY,\n  email text NOT NULL\n);",
          onChange: (doc) => handlers.current.change(doc),
          onSubmit: () => handlers.current.submit(),
        }),
      });
    });
    return () => {
      cancelled = true;
    };
  }, []);

  // Check the text once typing pauses; a newer check makes older answers moot.
  React.useEffect(() => {
    const seq = ++checkSeq.current;
    if (!text.trim()) return;
    const timer = window.setTimeout(() => {
      setCheck({ state: "checking" });
      compile.mutate(text, {
        onSuccess: (report) => {
          if (seq === checkSeq.current) setCheck({ state: "done", report });
        },
        onError: (error) => {
          if (seq !== checkSeq.current) return;
          if (error instanceof ApiError && error.code === "rate_limited") {
            setCheck({ state: "idle" });
          } else {
            setCheck({
              state: "unavailable",
              message:
                error instanceof ApiError && error.status === 503
                  ? error.message
                  : "Could not check the SQL. It is checked again when you upload.",
            });
          }
        },
      });
    }, CHECK_AFTER_MS);
    return () => window.clearTimeout(timer);
    // `compile` is a fresh object every render; its mutate is stable.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [text]);

  // An emptied editor has nothing to report, whatever the last check said.
  const shown: Check = text.trim() ? check : { state: "idle" };
  const diagnostics = React.useMemo(
    () => (text.trim() && check.state === "done" ? check.report.diagnostics : []),
    [check, text],
  );
  const errors = React.useMemo(
    () => diagnostics.filter((d) => d.severity === "error"),
    [diagnostics],
  );

  // Underline the errors in the text.
  React.useEffect(() => {
    if (!editor || !cm) return;
    cm.showMarks(
      editor.view,
      errors.flatMap((d) =>
        d.span ? [{ from: d.span.utf16_start, to: d.span.utf16_end, message: d.message }] : [],
      ),
    );
  }, [editor, cm, errors]);

  const loadFile = async (file: File) => {
    if (file.size > MAX_BYTES) {
      toast.error("This file is too large", {
        description: "A schema upload can be 1 MB at most.",
      });
      return;
    }
    const content = await file.text();
    setFileName(file.name);
    if (editor && cm) cm.replaceText(editor.view, content);
    else {
      setText(content);
      onDraftChange(content);
    }
  };

  const submit = () => {
    if (!text.trim() || upload.isPending) return;
    // A pending check is moot: the upload compiles again.
    checkSeq.current += 1;
    upload.mutate(
      { sql: text, message: message.trim() || undefined },
      {
        onSuccess: (version) => {
          onDraftChange("");
          onUploaded(version);
        },
        onError: (error) => {
          if (error instanceof ApiError && error.code === "compile_failed") {
            setCheck({
              state: "done",
              report: { diagnostics: diagnosticsOf(error.details), changes: null },
            });
          } else if (error instanceof ApiError && error.status === 503) {
            setCheck({ state: "unavailable", message: error.message });
          } else {
            toast.error("Could not upload", {
              description: error instanceof ApiError ? error.message : "Check your connection.",
            });
          }
        },
      },
    );
  };
  React.useEffect(() => {
    handlers.current.submit = submit;
  });

  const jump = (diagnostic: Diagnostic) => {
    if (!editor || !cm || !diagnostic.span) return;
    cm.jumpTo(editor.view, diagnostic.span.utf16_start, diagnostic.span.utf16_end);
  };

  return (
    <form
      className="flex min-h-0 flex-1 flex-col"
      onSubmit={(event) => {
        event.preventDefault();
        submit();
      }}
      onDragOver={(event) => {
        if (!event.dataTransfer.types.includes("Files")) return;
        event.preventDefault();
        setDragging(true);
      }}
      onDragLeave={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setDragging(false);
      }}
      onDrop={(event) => {
        event.preventDefault();
        setDragging(false);
        const file = event.dataTransfer.files[0];
        if (file) void loadFile(file);
      }}
    >
      <div className="flex items-start justify-between gap-4 px-5 pt-5 pb-4">
        <DialogHeader className="pr-0">
          <DialogTitle>Upload SQL</DialogTitle>
          <DialogDescription>
            Paste the schema or drop a .sql file. It is compiled in a sandbox Postgres and saved as
            v{next} only if it compiles.
          </DialogDescription>
        </DialogHeader>
        <FileButton
          size="sm"
          accept=".sql,text/plain,application/sql"
          onFile={(f) => void loadFile(f)}
        >
          <PageIcon />
          Open file
        </FileButton>
      </div>

      <div className="relative min-h-0 flex-1 border-y border-border">
        {loaded ? (
          <CodeView
            doc={draft}
            language="sql"
            label="SQL to upload"
            readOnly={false}
            extensions={loaded.extensions}
            active
            onEditor={setEditor}
          />
        ) : (
          <div className="h-full bg-card" />
        )}
        {dragging && (
          <div className="pointer-events-none absolute inset-2 flex items-center justify-center rounded-lg border-2 border-dashed border-primary bg-card/90 text-base font-medium text-foreground">
            Drop the file to replace the text
          </div>
        )}
      </div>

      <Status check={shown} errorCount={errors.length} fileName={fileName} empty={!text.trim()} />

      {diagnostics.length > 0 && (
        <ul
          aria-label="Problems"
          className="max-h-40 shrink-0 divide-y divide-border overflow-y-auto border-b border-border"
        >
          {diagnostics.map((d, i) => (
            <li key={i}>
              <Button
                variant="ghost"
                onClick={() => jump(d)}
                disabled={!d.span}
                className="h-auto w-full items-start justify-start gap-2.5 rounded-none px-5 py-2 text-left whitespace-normal disabled:opacity-100"
              >
                {d.severity === "error" ? (
                  <XmarkCircleIcon className="mt-0.5 text-destructive" />
                ) : (
                  <CheckCircleIcon className="mt-0.5 text-info" />
                )}
                <span className="grid min-w-0 gap-0.5">
                  <span className="text-sm text-foreground">
                    {d.span && (
                      <span className="mr-2 font-mono text-xs text-muted-foreground tabular-nums">
                        Line {d.span.from.line}:{d.span.from.column}
                      </span>
                    )}
                    {d.message}
                  </span>
                  {(d.hint ?? d.detail) && (
                    <span className="text-sm font-normal text-muted-foreground">
                      {d.hint ?? d.detail}
                    </span>
                  )}
                </span>
              </Button>
            </li>
          ))}
        </ul>
      )}

      <div className="flex flex-col gap-3 px-5 py-4 sm:flex-row sm:items-center">
        <Input
          aria-label="Message"
          placeholder="What changed? (optional)"
          maxLength={1000}
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          className="sm:flex-1"
        />
        <div className="flex flex-col-reverse gap-2 sm:flex-row">
          <Button onClick={() => onOpenChange(false)}>Cancel</Button>
          <Button
            type="submit"
            variant="primary"
            disabled={!text.trim()}
            loading={upload.isPending}
          >
            Create v{next}
            <KbdGroup
              keys={[modKey, "↵"]}
              className="ml-1 hidden sm:inline-flex [&_kbd]:border-primary-foreground/25 [&_kbd]:bg-primary-foreground/10 [&_kbd]:text-primary-foreground [&_kbd]:shadow-none"
            />
          </Button>
        </div>
      </div>
    </form>
  );
}

/** One line under the editor: what the last check found. */
function Status({
  check,
  errorCount,
  fileName,
  empty,
}: {
  check: Check;
  errorCount: number;
  fileName: string | null;
  empty: boolean;
}) {
  let content: React.ReactNode;
  if (empty) {
    content = <span className="text-muted-foreground">Paste SQL above, or open a .sql file.</span>;
  } else if (check.state === "checking") {
    content = (
      <>
        <Spinner className="size-3.5" />
        <span className="text-muted-foreground">Checking in the sandbox…</span>
      </>
    );
  } else if (check.state === "unavailable") {
    content = (
      <>
        <WarningTriangleIcon className="size-4 text-warning" />
        <span className="text-muted-foreground">{check.message}</span>
      </>
    );
  } else if (check.state === "done" && errorCount > 0) {
    content = (
      <>
        <XmarkCircleIcon className="size-4 text-destructive" />
        <span className="font-medium text-foreground">
          {errorCount === 1 ? "1 error" : `${errorCount} errors`}
        </span>
        <span className="text-muted-foreground">
          Fix {errorCount === 1 ? "it" : "them"} to upload.
        </span>
      </>
    );
  } else if (check.state === "done" && check.report.changes?.unchanged) {
    content = (
      <>
        <WarningTriangleIcon className="size-4 text-warning" />
        <span className="text-muted-foreground">
          Compiles, but it is the same schema as v{check.report.changes.against}.
        </span>
      </>
    );
  } else if (check.state === "done" && check.report.changes) {
    const { against, summary } = check.report.changes;
    content = (
      <>
        <CheckCircleIcon className="size-4 text-success" />
        <span className="font-medium text-foreground">Compiles</span>
        <span className="truncate text-muted-foreground">
          {summary.charAt(0).toUpperCase() + summary.slice(1)}
          {against ? ` since v${against}` : ""}.
        </span>
      </>
    );
  } else {
    content = <span className="text-muted-foreground">Checked when you pause typing.</span>;
  }
  return (
    <div
      role="status"
      aria-live="polite"
      className="flex h-9 shrink-0 items-center gap-2 border-b border-border px-5 text-sm"
    >
      {content}
      {fileName && (
        <span className="ml-auto hidden truncate font-mono text-xs text-faint-foreground sm:inline">
          {fileName}
        </span>
      )}
    </div>
  );
}
