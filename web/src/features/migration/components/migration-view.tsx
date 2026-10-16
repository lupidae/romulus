import { keepPreviousData, useQuery } from "@tanstack/react-query";
import * as React from "react";

import { ApiError } from "@/api/client";
import { CheckIcon, CopyIcon, DownloadIcon, NavArrowDownIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import { Tabs, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";
import { useMediaQuery, usePreference } from "@/features/schema/viewer/use-preference";
import { CodeView } from "@/features/schema/text/code-view";
import type { Highlights, Range } from "@/features/schema/text/codemirror";
import { useDebounced } from "@/lib/use-debounced";
import { cn } from "@/lib/utils";

import {
  basename,
  isArchive,
  isTool,
  lineRanges,
  type MigrationChoice,
  type MigrationFile,
  type MigrationHazard,
  type MigrationQuery,
  type MigrationWarning,
  NUMBERED,
  TOOLS,
} from "../queries";
import { save, zip } from "../zip";

export interface MigrationViewProps {
  /** The query for a tool and options: between two versions, or for a draft. */
  query: (choice: MigrationChoice) => MigrationQuery;
  /** Download is the view's primary action, unless the view sits in a dialog that has its own. */
  primary?: boolean;
  className?: string;
}

const isBoolean = (value: unknown): value is boolean => typeof value === "boolean";

/** Warnings shown before "Show N more". */
const FIRST_WARNINGS = 2;

/**
 * A migration, ready to review and take away: the tool's files with the
 * statements that delete data tinted, what to check before running it, and
 * Copy and Download. The tool and the concurrently option are remembered
 * per browser. Self-contained (no router): the diff page and a draft's
 * publish dialog both render it.
 */
export function MigrationView({ query, primary = true, className }: MigrationViewProps) {
  const [format, setFormat] = usePreference("romulus:migration-tool", "plain", isTool);
  const [concurrently, setConcurrently] = usePreference(
    "romulus:migration-concurrently",
    false,
    isBoolean,
  );
  // The first file's version, for tools that number their migrations: sent
  // once it reads as one, after a pause in typing.
  const numbered = NUMBERED[format];
  const [versionText, setVersionText] = React.useState("");
  const [version, setVersion] = React.useState<string | undefined>(undefined);
  const commitVersion = useDebounced((text: string) => {
    const trimmed = text.trim();
    if (trimmed === "") setVersion(undefined);
    else if (numbered?.pattern.test(trimmed)) setVersion(trimmed);
  }, 400);
  const versionInvalid =
    numbered !== undefined &&
    versionText.trim() !== "" &&
    !numbered.pattern.test(versionText.trim());
  const migration = useQuery({
    ...query({ format, concurrently, version: numbered ? version : undefined }),
    // Another tool keeps the last files on screen while the new ones load.
    placeholderData: keepPreviousData,
  });
  const data = migration.data;
  const uid = React.useId();

  const [fileIndex, setFileIndex] = React.useState(0);
  const files = data?.files ?? [];
  const file: MigrationFile | undefined = files[Math.min(fileIndex, files.length - 1)];

  // Where the flagged statements are: deletes data (red), can fail on
  // existing rows (amber), widens access (blue).
  const ranges = React.useMemo(() => {
    const of = (...hazards: MigrationHazard[]) =>
      file
        ? lineRanges(
            file.content,
            file.marks.filter((m) => hazards.includes(m.hazard)),
          )
        : [];
    return {
      danger: of("deletes_data"),
      caution: of("can_fail"),
      access: of("widens_access"),
      destructive: of("deletes_data", "can_fail"),
    };
  }, [file]);
  const highlights = React.useMemo<Highlights>(
    () => ({
      related: [],
      danger: ranges.danger,
      caution: ranges.caution,
      notice: ranges.access,
    }),
    [ranges],
  );
  // The walkers: through the destructive statements, through what widens access.
  const [reveal, setReveal] = React.useState<{
    kind: "destructive" | "access";
    range: Range;
    n: number;
  } | null>(null);
  const walk = (kind: "destructive" | "access") => {
    const list = ranges[kind];
    if (list.length === 0) return;
    const n = reveal?.kind === kind ? (reveal.n + 1) % list.length : 0;
    setReveal({ kind, range: list[n]!, n });
  };

  const [copied, setCopied] = React.useState(false);
  React.useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 2000);
    return () => window.clearTimeout(timer);
  }, [copied]);
  const copy = async () => {
    if (!file) return;
    try {
      await navigator.clipboard.writeText(file.content);
      setCopied(true);
    } catch {
      toast.error("Could not copy", { description: "Select the text and press ⌘C or Ctrl+C." });
    }
  };

  const archive = isArchive(files);
  const download = () => {
    if (!data || files.length === 0) return;
    if (archive) save(data.archive, zip(files));
    else save(files[0]!.path, files[0]!.content);
  };

  const statements = data?.statements.length ?? 0;
  const loading = migration.isPending;
  const switching = migration.isFetching && migration.isPlaceholderData;

  return (
    // Scrolls as a whole when the warnings and the toolbar leave the SQL too
    // little room (a phone): the editor keeps a usable height.
    <div className={cn("flex h-full min-h-0 flex-col overflow-y-auto bg-card", className)}>
      <div className="flex shrink-0 flex-wrap items-center gap-x-4 gap-y-2 border-b border-border px-3 py-2">
        <div className="flex items-center gap-2">
          <Label htmlFor={`${uid}-tool`} className="text-muted-foreground">
            Tool
          </Label>
          <Select
            value={format}
            onValueChange={(value) => {
              if (!isTool(value)) return;
              setFormat(value);
              setFileIndex(0);
              setReveal(null);
              // Each tool numbers its own way.
              setVersionText("");
              setVersion(undefined);
            }}
          >
            <SelectTrigger id={`${uid}-tool`} className="w-auto min-w-36">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {TOOLS.map((tool) => (
                <SelectItem key={tool.value} value={tool.value} description={tool.description}>
                  {tool.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
        <div className="flex items-center gap-2">
          <Checkbox
            id={`${uid}-concurrently`}
            checked={concurrently}
            onCheckedChange={(checked) => {
              setConcurrently(checked === true);
              setFileIndex(0);
              setReveal(null);
            }}
          />
          <Tooltip content="Indexes on existing tables are built without blocking writes, after the transaction: CONCURRENTLY cannot run inside one.">
            <Label htmlFor={`${uid}-concurrently`} className="font-normal">
              Build indexes concurrently
            </Label>
          </Tooltip>
        </div>
        {numbered && (
          <div className="flex items-center gap-2">
            <Label htmlFor={`${uid}-version`} className="text-muted-foreground">
              Version
            </Label>
            <Tooltip
              content={`The first file's version, if yours are numbered: ${numbered.hint}. Empty: ${format === "flyway" ? "the version's number" : "the time"}.`}
            >
              <Input
                id={`${uid}-version`}
                value={versionText}
                onChange={(event) => {
                  setVersionText(event.target.value);
                  commitVersion(event.target.value);
                }}
                placeholder={format === "flyway" ? String(data?.to ?? "") : "timestamp"}
                inputMode="numeric"
                aria-invalid={versionInvalid || undefined}
                aria-describedby={versionInvalid ? `${uid}-version-error` : undefined}
                className="h-9 w-28 font-mono sm:h-8"
              />
            </Tooltip>
            {versionInvalid && (
              <span id={`${uid}-version-error`} role="alert" className="text-sm text-destructive">
                {numbered.hint}
              </span>
            )}
          </div>
        )}
        <div className="ml-auto flex flex-wrap items-center gap-2">
          {switching && <Spinner className="size-3.5 text-muted-foreground" label="Updating" />}
          {data && (
            <span className="text-sm text-muted-foreground tabular-nums">
              {statements === 0
                ? "Nothing to run"
                : `${statements} statement${statements === 1 ? "" : "s"}`}
            </span>
          )}
          {ranges.destructive.length > 0 && (
            <Tooltip content="Scroll to the next statement in this file that deletes data (red) or can fail on existing rows (amber)">
              <Button
                size="sm"
                variant="ghost"
                className="text-destructive hover:text-destructive"
                onClick={() => walk("destructive")}
                aria-label={`${ranges.destructive.length} destructive: scroll to the next one`}
              >
                <Badge variant="destructive" className="tabular-nums">
                  {reveal?.kind === "destructive"
                    ? `${reveal.n + 1}/${ranges.destructive.length}`
                    : ranges.destructive.length}
                </Badge>
                destructive
                <NavArrowDownIcon />
              </Button>
            </Tooltip>
          )}
          {ranges.access.length > 0 && (
            <Tooltip content="Scroll to the next statement in this file that lets more rows through to more roles">
              <Button
                size="sm"
                variant="ghost"
                className="text-info hover:text-info"
                onClick={() => walk("access")}
                aria-label={`${ranges.access.length} widen access: scroll to the next one`}
              >
                <Badge variant="info" className="tabular-nums">
                  {reveal?.kind === "access"
                    ? `${reveal.n + 1}/${ranges.access.length}`
                    : ranges.access.length}
                </Badge>
                widen access
                <NavArrowDownIcon />
              </Button>
            </Tooltip>
          )}
          <Button
            size="sm"
            onClick={() => void copy()}
            disabled={!file}
            aria-label={copied ? "Copied" : `Copy ${file ? basename(file.path) : "the file"}`}
          >
            {copied ? <CheckIcon className="text-success" /> : <CopyIcon />}
            {copied ? "Copied" : "Copy"}
          </Button>
          <Button
            size="sm"
            variant={primary ? "primary" : "secondary"}
            onClick={download}
            disabled={files.length === 0}
          >
            <DownloadIcon />
            {archive ? "Download .zip" : "Download"}
          </Button>
        </div>
        <span role="status" aria-live="polite" className="sr-only">
          {copied ? "Copied to the clipboard" : ""}
        </span>
      </div>

      {migration.isError ? (
        <div className="p-3">
          <Alert
            variant="destructive"
            title="Could not prepare the migration"
            action={
              <Button
                size="sm"
                loading={migration.isFetching}
                onClick={() => void migration.refetch()}
              >
                Retry
              </Button>
            }
          >
            {migration.error instanceof ApiError
              ? migration.error.message
              : "Check your connection and try again."}
          </Alert>
        </div>
      ) : loading || !data ? (
        <div
          aria-busy
          aria-label="Preparing the migration"
          className="grid content-start gap-2.5 px-4 py-3"
        >
          {[70, 52, 84, 40, 76, 60, 90, 46].map((w, i) => (
            <Skeleton key={i} className="h-3" style={{ width: `${w}%` }} />
          ))}
        </div>
      ) : (
        <>
          {data.warnings.length > 0 && <Warnings warnings={data.warnings} />}
          {files.length > 1 && (
            <div className="shrink-0 overflow-x-auto border-b border-border px-3 py-1.5">
              <Tabs
                value={String(Math.min(fileIndex, files.length - 1))}
                onValueChange={(value) => {
                  setFileIndex(Number(value));
                  setReveal(null);
                }}
              >
                <TabsList aria-label="Files">
                  {files.map((f, i) => (
                    <TabsTrigger key={f.path} value={String(i)} className="font-mono text-xs">
                      {basename(f.path)}
                      {f.role === "down" && <Badge variant="outline">down</Badge>}
                    </TabsTrigger>
                  ))}
                </TabsList>
              </Tabs>
            </div>
          )}
          {file && (files.length === 1 || file.path.includes("/")) && (
            <div className="flex shrink-0 items-center gap-2 px-3 pt-2 pb-1 text-xs text-muted-foreground">
              <span className="truncate font-mono" title={file.path}>
                {file.path}
              </span>
              {file.role === "down" && <span>· best-effort down migration</span>}
            </div>
          )}
          {file && (
            <CodeView
              // A new file is a new editor: its scroll starts at the top.
              key={`${data.format}:${file.path}`}
              doc={file.content}
              language="sql"
              label={`Migration file ${file.path}`}
              readOnly
              active
              highlights={highlights}
              reveal={reveal?.range ?? null}
              className="min-h-[min(28rem,70dvh)] flex-1"
            />
          )}
        </>
      )}
    </div>
  );
}

/** What to check before running it: the first two, then the rest on demand. On a phone, the title alone until asked. */
function Warnings({ warnings }: { warnings: MigrationWarning[] }) {
  const narrow = useMediaQuery("(max-width: 40rem)");
  const [shown, setShown] = React.useState<number | null>(null);
  const count = shown ?? (narrow ? 0 : Math.min(FIRST_WARNINGS, warnings.length));
  const more = warnings.length - count;
  return (
    <div className="shrink-0 border-b border-border p-3">
      <Alert
        variant="warning"
        title={
          warnings.length === 1
            ? "One thing to check before running it"
            : `${warnings.length} things to check before running it`
        }
      >
        {count > 0 && (
          <ul className="mt-1 grid list-disc gap-1 pl-4">
            {warnings.slice(0, count).map((warning, i) => (
              <li key={i} className="text-sm">
                {warning.message}
              </li>
            ))}
          </ul>
        )}
        {more > 0 ? (
          <Button
            size="sm"
            variant="link"
            className="mt-1.5"
            onClick={() => setShown(warnings.length)}
          >
            {count === 0 ? "Show them" : `Show ${more} more`}
          </Button>
        ) : (
          warnings.length > FIRST_WARNINGS && (
            <Button
              size="sm"
              variant="link"
              className="mt-1.5"
              onClick={() => setShown(FIRST_WARNINGS)}
            >
              Show fewer
            </Button>
          )
        )}
      </Alert>
    </div>
  );
}
