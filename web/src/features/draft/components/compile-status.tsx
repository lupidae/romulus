import { CheckCircleIcon, InfoCircleIcon, WarningTriangleIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import type { Diagnostic } from "../queries";
import type { CompileView } from "../session";

interface CompileStatusProps {
  compile: CompileView;
  /** Puts the caret on a diagnostic in the SQL. */
  onJump: (diagnostic: Diagnostic) => void;
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/**
 * Whether the SQL compiles, in the editor's bar: "Compiling…", "Compiled",
 * "2 errors", "Not checked". The details (each error, where it is) open
 * below; a click on one goes to it in the text.
 */
export function CompileStatus({ compile, onJump }: CompileStatusProps) {
  const errors = compile.diagnostics.filter((d) => d.severity === "error");
  const notes = compile.diagnostics.filter((d) => d.severity === "info");
  const busy = compile.stale || compile.state === "waiting" || compile.state === "pending";

  let icon: React.ReactNode;
  let label: string;
  let tone = "text-muted-foreground";
  if (busy && compile.state !== "failed") {
    icon = <Spinner label="Compiling" className="size-3.5" />;
    label = "Compiling…";
  } else if (compile.state === "failed") {
    icon = busy ? (
      <Spinner label="Compiling" className="size-3.5" />
    ) : (
      <WarningTriangleIcon className="text-destructive" />
    );
    label = busy ? "Compiling…" : plural(errors.length || 1, "error", "errors");
    tone = busy ? tone : "text-destructive";
  } else if (compile.state === "unavailable") {
    icon = <WarningTriangleIcon className="text-warning" />;
    label = "Not checked";
  } else {
    icon = <CheckCircleIcon className="text-success" />;
    label = "Compiled";
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
          <span className="truncate">{label}</span>
        </Button>
      </PopoverTrigger>
      <PopoverContent align="start" className="grid w-96 max-w-[calc(100vw-2rem)] gap-3">
        <Details compile={compile} errors={errors} notes={notes} busy={busy} onJump={onJump} />
      </PopoverContent>
    </Popover>
  );
}

function Details({
  compile,
  errors,
  notes,
  busy,
  onJump,
}: {
  compile: CompileView;
  errors: Diagnostic[];
  notes: Diagnostic[];
  busy: boolean;
  onJump: (diagnostic: Diagnostic) => void;
}) {
  if (compile.state === "unavailable") {
    return (
      <Explain title="The SQL could not be checked">
        The compiler is not reachable right now. Your edits are saved and will be checked as soon as
        it is back.
      </Explain>
    );
  }
  if (compile.state !== "failed") {
    return (
      <>
        <Explain title={busy ? "Compiling your latest edits" : "The SQL compiles"}>
          {busy
            ? "Postgres checks the text a moment after typing stops; the graph follows."
            : "Postgres accepts every statement; the graph shows exactly this schema."}
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
            ? "Compiling your latest edits"
            : `${plural(errors.length || 1, "error", "errors")} in the SQL`
        }
      >
        Until it compiles, the graph shows the last version that did.
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
            className="h-auto w-full items-start justify-start gap-2 px-1.5 py-1.5 text-left whitespace-normal"
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
