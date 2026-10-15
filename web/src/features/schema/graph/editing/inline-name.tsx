import { useEffect, useId, useRef, useState } from "react";

import { Input } from "@/components/ui/input";
import { Popover, PopoverAnchor, PopoverContent } from "@/components/ui/popover";
import { cn } from "@/lib/utils";

interface InlineNameProps {
  /** The name now: what the field starts with, all selected. */
  initial: string;
  /** Why a name cannot be used, or null. Not called for the initial name. */
  validate: (name: string) => string | null;
  onCommit: (name: string) => void;
  onCancel: () => void;
  /** Saving the initial name as it is counts (a new table's suggested name). */
  acceptInitial?: boolean;
  "aria-label": string;
  className?: string;
}

/**
 * A name edited where it is drawn (a table's header, a column's row): Enter
 * or leaving the field saves, Esc cancels. A name that cannot be used says
 * why beside the field and is not saved.
 */
export function InlineName({
  initial,
  validate,
  onCommit,
  onCancel,
  acceptInitial = false,
  className,
  ...props
}: InlineNameProps) {
  const [value, setValue] = useState(initial);
  const [shown, setShown] = useState<string | null>(null);
  const input = useRef<HTMLInputElement>(null);
  const done = useRef(false);
  const problemId = useId();

  useEffect(() => {
    const el = input.current;
    if (!el) return;
    el.focus({ preventScroll: true });
    el.select();
  }, []);

  const name = value.trim();
  const unchanged = name === initial && !acceptInitial;
  const problem = unchanged ? null : validate(name);

  const finish = (save: boolean) => {
    if (done.current) return;
    if (save && problem) {
      setShown(problem);
      return;
    }
    done.current = true;
    if (save && !unchanged) onCommit(name);
    else onCancel();
  };

  return (
    <Popover open={shown !== null}>
      <PopoverAnchor asChild>
        <Input
          ref={input}
          value={value}
          spellCheck={false}
          autoComplete="off"
          aria-invalid={shown ? true : undefined}
          aria-describedby={shown ? problemId : undefined}
          onChange={(event) => {
            setValue(event.target.value);
            setShown(null);
          }}
          onKeyDown={(event) => {
            event.stopPropagation();
            if (event.key === "Enter") {
              event.preventDefault();
              finish(true);
            } else if (event.key === "Escape") {
              event.preventDefault();
              finish(false);
            }
          }}
          // Leaving with a name that cannot be used keeps the old one.
          onBlur={() => finish(!problem)}
          // Clicks inside stay with the field (no drag, no selection change).
          onMouseDown={(event) => event.stopPropagation()}
          onDoubleClick={(event) => event.stopPropagation()}
          className={cn(
            "nodrag nopan nowheel -mx-1 h-6 w-auto min-w-0 flex-1 rounded-sm border-ring px-1 shadow-none sm:h-6",
            "ring-3 ring-ring/20 aria-invalid:border-destructive aria-invalid:ring-destructive/20",
            className,
          )}
          {...props}
        />
      </PopoverAnchor>
      <PopoverContent
        id={problemId}
        role="alert"
        side="bottom"
        sideOffset={6}
        // The field keeps the focus.
        onOpenAutoFocus={(event) => event.preventDefault()}
        onCloseAutoFocus={(event) => event.preventDefault()}
        className="w-auto max-w-64 border-destructive/40 px-2.5 py-1.5 text-sm text-destructive"
      >
        {shown}
      </PopoverContent>
    </Popover>
  );
}
