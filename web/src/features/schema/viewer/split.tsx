import { type ReactNode, useRef } from "react";

import { cn } from "@/lib/utils";

export const MIN_RATIO = 0.25;
export const MAX_RATIO = 0.8;
/** The graph is the primary pane. */
export const DEFAULT_RATIO = 0.62;

const clamp = (r: number) => Math.min(MAX_RATIO, Math.max(MIN_RATIO, r));

interface SplitProps {
  /** Share of the width given to the start pane. */
  ratio: number;
  /** Which panes are shown. Both stay mounted either way: hiding one keeps its state. */
  show?: "both" | "start" | "end";
  onRatioChange: (ratio: number) => void;
  start: ReactNode;
  end: ReactNode;
  /** Name of what the handle resizes, for assistive tech. */
  label: string;
  /** Attributes for each pane's element (ids and roles when tabs switch them). */
  startProps?: React.HTMLAttributes<HTMLDivElement>;
  endProps?: React.HTMLAttributes<HTMLDivElement>;
  className?: string;
}

/**
 * Two panes side by side and the handle between them: drag it, or focus it
 * and use the arrow keys (Home and End for the limits, Enter to reset), as in
 * the WAI-ARIA window splitter pattern. Double-click resets too.
 */
export function Split({
  ratio,
  show = "both",
  onRatioChange,
  start,
  end,
  label,
  startProps,
  endProps,
  className,
}: SplitProps) {
  const root = useRef<HTMLDivElement>(null);
  const drag = useRef<{ left: number; width: number } | null>(null);

  const onPointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (event.button !== 0 || !root.current) return;
    const rect = root.current.getBoundingClientRect();
    drag.current = { left: rect.left, width: rect.width };
    event.currentTarget.setPointerCapture(event.pointerId);
    event.preventDefault();
  };
  const onPointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!drag.current) return;
    onRatioChange(clamp((event.clientX - drag.current.left) / drag.current.width));
  };
  const onPointerUp = (event: React.PointerEvent<HTMLDivElement>) => {
    drag.current = null;
    event.currentTarget.releasePointerCapture(event.pointerId);
  };
  const onKeyDown = (event: React.KeyboardEvent) => {
    const step = event.shiftKey ? 0.1 : 0.025;
    const next =
      event.key === "ArrowLeft"
        ? ratio - step
        : event.key === "ArrowRight"
          ? ratio + step
          : event.key === "Home"
            ? MIN_RATIO
            : event.key === "End"
              ? MAX_RATIO
              : event.key === "Enter"
                ? DEFAULT_RATIO
                : null;
    if (next === null) return;
    event.preventDefault();
    onRatioChange(clamp(next));
  };

  return (
    <div ref={root} className={cn("flex h-full min-h-0 w-full", className)}>
      <div
        {...startProps}
        hidden={show === "end"}
        className="min-w-0"
        // Alone, a pane takes the whole width (a grow under 1 would leave a gap).
        style={{ flex: show === "both" ? `${ratio} 1 0%` : "1 1 0%" }}
      >
        {start}
      </div>
      {/* A focusable separator is the WAI-ARIA window splitter, an
          interactive widget that jsx-a11y counts as static. */}
      {/* eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions */}
      <div
        hidden={show !== "both"}
        role="separator"
        aria-orientation="vertical"
        aria-label={label}
        aria-valuenow={Math.round(ratio * 100)}
        aria-valuemin={MIN_RATIO * 100}
        aria-valuemax={MAX_RATIO * 100}
        // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
        tabIndex={0}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={onPointerUp}
        onKeyDown={onKeyDown}
        onDoubleClick={() => onRatioChange(DEFAULT_RATIO)}
        className={cn(
          "group relative z-(--z-sticky) w-px shrink-0 cursor-col-resize touch-none bg-border outline-none select-none",
          // A wider invisible grip, and a visible one on hover and focus.
          "before:absolute before:inset-y-0 before:-left-1.5 before:w-3",
          "pointer-coarse:before:-left-5.5 pointer-coarse:before:w-11",
          "after:absolute after:inset-y-0 after:-left-px after:w-0.75 after:bg-primary after:opacity-0 after:transition-opacity",
          "hover:after:opacity-60 focus-visible:after:opacity-100 active:after:opacity-100",
        )}
      />
      <div
        {...endProps}
        hidden={show === "start"}
        className="min-w-0"
        style={{ flex: show === "both" ? `${1 - ratio} 1 0%` : "1 1 0%" }}
      >
        {end}
      </div>
    </div>
  );
}
