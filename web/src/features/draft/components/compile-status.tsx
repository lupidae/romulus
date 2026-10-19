import { CheckCircleIcon, InfoCircleIcon, WarningTriangleIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import type { Diagnostic } from "../queries";
import type { CompileView } from "../session";

interface CompileStatusProps {
  compile: CompileView;
  /** The Postgres major it compiles on (the database's), once known. */
  major?: number;
  /** Puts the caret on a diagnostic in the SQL. */
  onJump: (diagnostic: Diagnostic) => void;
  /**
   * What checks the SQL: the app's Postgres sandbox, or Postgres's parser
   * alone (the homepage's hero, in the browser), which is said so.
   */
  checker?: "sandbox" | "parser";
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/**
 * Whether the SQL compiles, in the editor's bar: "Compiling…", "Compiled",
 * "2 errors", "Not checked". The details (each error, where it is) open
 * below; a click on one goes to it in the text.
 */
export function CompileStatus({ compile, major, onJump, checker = "sandbox" }: CompileStatusProps) {
  const parser = checker === "parser";
  const errors = compile.diagnostics.filter((d) => d.severity === "error");
  const problems = errorCount(errors);
  const notes = compile.diagnostics.filter((d) => d.severity === "info");
  const busy = compile.stale || compile.state === "waiting" || compile.state === "pending";

  let icon: React.ReactNode;
  let label: string;
  // The parser's notes (what the browser preview leaves out) are worth a word.
  let suffix: string | null = null;
  let tone = "text-muted-foreground";
  if (busy && compile.state !== "failed") {
    icon = <Spinner label={parser ? "Checking" : "Compiling"} className="size-3.5" />;
    label = parser ? "Checking…" : "Compiling…";
  } else if (compile.state === "failed") {
    icon = busy ? (
      <Spinner label={parser ? "Checking" : "Compiling"} className="size-3.5" />
    ) : (
      <WarningTriangleIcon className="text-destructive" />
    );
    label = busy ? (parser ? "Checking…" : "Compiling…") : problems;
    tone = busy ? tone : "text-destructive";
  } else if (compile.state === "unavailable") {
    icon = <WarningTriangleIcon className="text-warning" />;
    label = "Not checked";
  } else if (parser && notes.length > 0) {
    icon = <InfoCircleIcon className="text-info" />;
    label = "Checked";
    suffix = `, ${notes.length} ${notes.length === 1 ? "note" : "notes"}`;
  } else {
    icon = <CheckCircleIcon className="text-success" />;
    label = parser ? "Checked" : "Compiled";
  }

  return (
    <Popover>
      <PopoverTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          data-testid="compile-status"
          aria-live="polite"
          className={cn("min-w-0 gap-1.5 font-medium", tone)}
        >
          {icon}
          <span className="truncate">
            {label}
            {suffix && <span className="max-sm:sr-only">{suffix}</span>}
          </span>
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="grid w-96 max-w-[calc(100vw-2rem)] gap-3">
        <Details
          compile={compile}
          errors={errors}
          notes={notes}
          busy={busy}
          postgres={major ? `Postgres ${major}` : "Postgres"}
          parser={parser}
          onJump={onJump}
        />
      </PopoverContent>
    </Popover>
  );
}

function Details({
  compile,
  errors,
  notes,
  busy,
  postgres,
  parser,
  onJump,
}: {
  compile: CompileView;
  errors: Diagnostic[];
  notes: Diagnostic[];
  busy: boolean;
  /** "Postgres 15": what it compiles on. */
  postgres: string;
  /** Checked by the parser in the browser, not compiled in Postgres. */
  parser: boolean;
  onJump: (diagnostic: Diagnostic) => void;
}) {
  if (compile.state === "unavailable") {
    return (
      <Explain title="The SQL could not be checked">
        The compiler is unreachable. Your edits are saved and checked once it is back.
      </Explain>
    );
  }
  if (parser && compile.state !== "failed") {
    return (
      <>
        <Explain title={busy ? "Checking your latest edits" : "The SQL checks out"}>
          {busy
            ? `${postgres}'s parser reads the text in your browser a moment after typing stops; the graph follows.`
            : notes.length > 0
              ? `${postgres}'s parser reads every statement; this preview builds the schema from them, except what is below. Signed in, romulus compiles everything in a real ${postgres}.`
              : `${postgres}'s parser reads every statement, and this preview builds the schema from them: the graph shows it. Signed in, romulus compiles it in a real ${postgres}, which checks what a parser cannot.`}
        </Explain>
        {notes.length > 0 && <List items={notes} onJump={onJump} />}
      </>
    );
  }
  if (compile.state !== "failed") {
    return (
      <>
        <Explain title={busy ? "Compiling your latest edits" : "The SQL compiles"}>
          {busy
            ? `${postgres} checks the text a moment after typing stops; the graph follows.`
            : `${postgres} accepts every statement; the graph shows exactly this schema.`}
        </Explain>
        {notes.length > 0 && <List items={notes} onJump={onJump} />}
      </>
    );
  }
  return (
    <>
      <Explain
        title={
          busy
            ? `${parser ? "Checking" : "Compiling"} your latest edits`
            : `${errorCount(errors)} in the SQL`
        }
      >
        {parser
          ? "Until it is fixed, the graph shows the last version that worked."
          : "Until it compiles, the graph shows the last version that did."}
      </Explain>
      <List items={errors} onJump={onJump} />
    </>
  );
}

function Explain({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="grid gap-0.5">
      <p className="text-sm font-semibold text-foreground">{title}</p>
      <p className="text-sm text-muted-foreground">{children}</p>
    </div>
  );
}

function List({
  items,
  onJump,
}: {
  items: Diagnostic[];
  onJump: (diagnostic: Diagnostic) => void;
}) {
  return (
    <ul className="-mx-1.5 grid max-h-72 gap-0.5 overflow-y-auto">
      {items.map((d, i) => (
        <li key={i}>
          <Button
            variant="ghost"
            onClick={() => onJump(d)}
            className="h-auto w-full items-start justify-start gap-2 px-1.5 py-1.5 text-left whitespace-normal sm:h-auto"
          >
            {d.severity === "error" ? (
              <WarningTriangleIcon className="mt-0.5 text-destructive" />
            ) : (
              <InfoCircleIcon className="mt-0.5 text-info" />
            )}
            <span className="grid min-w-0 gap-0.5">
              <span className="text-sm font-normal text-foreground">{d.message}</span>
              {d.hint && (
                <span className="text-sm font-normal text-muted-foreground">{d.hint}</span>
              )}
              {d.span && (
                <span className="font-mono text-xs font-normal text-faint-foreground tabular">
                  Line {d.span.from.line}, column {d.span.from.column}
                </span>
              )}
            </span>
          </Button>
        </li>
      ))}
    </ul>
  );
}

/** "2 errors", or "1 conflict" when what stops the compile is conflict blocks only. */
function errorCount(errors: Diagnostic[]) {
  const n = errors.length || 1;
  return errors.length > 0 && errors.every((d) => d.code === "conflict")
    ? plural(n, "conflict", "conflicts")
    : plural(n, "error", "errors");
}
