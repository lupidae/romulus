import { useReactFlow, useStore } from "@xyflow/react";

import {
  FingerprintIcon,
  FrameSelectIcon,
  HashtagIcon,
  InfoCircleIcon,
  KeyIcon,
  LinkIcon,
  SearchIcon,
  SparksIcon,
  ZoomInIcon,
  ZoomOutIcon,
} from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Kbd, KbdGroup } from "@/components/ui/kbd";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tooltip } from "@/components/ui/tooltip";
import { modKey } from "@/lib/platform";
import { cn } from "@/lib/utils";

import { type ChangeStatus, statusGlyph } from "../diff";
import type { DiffCounts } from "./build";
import { EndMark } from "./relation-edge";

/** Floating toolbars share one look: a small raised card of ghost buttons. */
export const toolbarClass =
  "flex items-center gap-0.5 rounded-lg border border-border bg-card p-0.5 shadow-raised";

export function SearchButton({ onClick }: { onClick: () => void }) {
  return (
    <Button
      variant="secondary"
      onClick={onClick}
      className="w-9 justify-start gap-2 px-2.5 text-muted-foreground shadow-raised sm:w-56"
      aria-keyshortcuts="Meta+K Control+K /"
    >
      <SearchIcon />
      <span className="hidden flex-1 text-left font-normal sm:inline">Find a table</span>
      <KbdGroup keys={[modKey, "K"]} className="hidden sm:inline-flex" />
      <span className="sr-only sm:hidden">Find a table</span>
    </Button>
  );
}

export function ZoomControls() {
  const { zoomIn, zoomOut, fitView, zoomTo } = useReactFlow();
  const zoom = useStore((s) => s.transform[2]);
  return (
    <div className={toolbarClass}>
      <Tooltip content="Zoom out" shortcut={["−"]} side="top">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Zoom out"
          onClick={() => void zoomOut({ duration: 200 })}
        >
          <ZoomOutIcon />
        </Button>
      </Tooltip>
      <Tooltip content="Actual size" side="top">
        <Button
          variant="ghost"
          size="sm"
          aria-label={`Zoom ${Math.round(zoom * 100)}%, reset to 100%`}
          onClick={() => void zoomTo(1, { duration: 200 })}
          className="w-12 px-0 font-normal tabular"
        >
          {Math.round(zoom * 100)}%
        </Button>
      </Tooltip>
      <Tooltip content="Zoom in" shortcut={["+"]} side="top">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Zoom in"
          onClick={() => void zoomIn({ duration: 200 })}
        >
          <ZoomInIcon />
        </Button>
      </Tooltip>
      <Tooltip content="Fit to screen" shortcut={["0"]} side="top">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Fit to screen"
          onClick={() => void fitView({ padding: 0.12, duration: 300, maxZoom: 1 })}
        >
          <FrameSelectIcon />
        </Button>
      </Tooltip>
    </div>
  );
}

const STATUS_TEXT: Record<ChangeStatus, string> = {
  added: "text-success",
  removed: "text-destructive",
  changed: "text-warning",
};

/** Counts per status; each steps through the tables with that status. */
export function DiffLegend({
  counts,
  onNext,
}: {
  counts: DiffCounts;
  onNext: (status: ChangeStatus) => void;
}) {
  const statuses = (["added", "removed", "changed"] as const).filter((s) => counts[s] > 0);
  if (!statuses.length) {
    return (
      <div className={cn(toolbarClass, "h-8 px-2.5 text-sm text-muted-foreground")}>
        No table changed
      </div>
    );
  }
  return (
    <div className={toolbarClass} role="group" aria-label="Changes">
      {statuses.map((status) => (
        <Tooltip key={status} content={`Show the next ${status} table`} side="bottom">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => onNext(status)}
            className="gap-1 px-2 font-normal"
          >
            <span aria-hidden className={cn("w-2.5 font-mono font-semibold", STATUS_TEXT[status])}>
              {statusGlyph(status)}
            </span>
            <span className="text-foreground tabular">{counts[status]}</span>
            <span className="text-muted-foreground">{status}</span>
          </Button>
        </Tooltip>
      ))}
    </div>
  );
}

function Mark({ mark }: { mark: "||" | "|o" | "o{" }) {
  return (
    <svg
      aria-hidden
      viewBox="0 0 40 16"
      className="h-4 w-10 shrink-0 fill-none stroke-foreground stroke-[1.5]"
    >
      <path d="M 0 8 L 40 8" />
      <EndMark x={40} y={8} dir={-1} mark={mark} />
    </svg>
  );
}

function KeyRow({ icon, children }: { icon: React.ReactNode; children: React.ReactNode }) {
  return (
    <li className="flex items-center gap-2.5">
      <span className="flex w-10 shrink-0 justify-center text-muted-foreground [&_svg]:size-3.5">
        {icon}
      </span>
      <span>{children}</span>
    </li>
  );
}

/** What the marks on the graph mean, and its keyboard shortcuts. */
export function GraphKey({ diff }: { diff: boolean }) {
  return (
    <Popover>
      <Tooltip content="Legend and shortcuts" side="bottom">
        <PopoverTrigger asChild>
          <Button variant="ghost" size="icon-sm" aria-label="Legend and shortcuts">
            <InfoCircleIcon />
          </Button>
        </PopoverTrigger>
      </Tooltip>
      <PopoverContent
        align="end"
        className="grid max-h-[min(70vh,34rem)] w-76 gap-4 overflow-y-auto text-sm"
      >
        <section className="grid gap-2">
          <h3 className="text-xs font-medium text-muted-foreground">Columns</h3>
          <ul className="grid gap-1.5">
            <KeyRow icon={<KeyIcon className="text-primary" />}>Primary key</KeyRow>
            <KeyRow icon={<LinkIcon className="text-info" />}>Foreign key</KeyRow>
            <KeyRow icon={<FingerprintIcon />}>Unique</KeyRow>
            <KeyRow icon={<HashtagIcon />}>Identity</KeyRow>
            <KeyRow icon={<SparksIcon />}>Generated</KeyRow>
            <KeyRow icon={<span className="font-mono text-xs">text?</span>}>Nullable</KeyRow>
            <KeyRow
              icon={
                <span className="font-mono text-xs text-syntax-type underline decoration-dotted">
                  enum
                </span>
              }
            >
              Enum, domain or composite type
            </KeyRow>
          </ul>
        </section>
        <section className="grid gap-2">
          <h3 className="text-xs font-medium text-muted-foreground">Relationships, at each end</h3>
          <ul className="grid gap-1.5">
            <KeyRow icon={<Mark mark="||" />}>Exactly one</KeyRow>
            <KeyRow icon={<Mark mark="|o" />}>Zero or one</KeyRow>
            <KeyRow icon={<Mark mark="o{" />}>Zero or many</KeyRow>
            <KeyRow
              icon={
                <svg
                  aria-hidden
                  viewBox="0 0 40 16"
                  className="h-4 w-10 fill-none stroke-foreground stroke-[1.5]"
                >
                  <path d="M 0 8 L 40 8" strokeDasharray="5 4" />
                </svg>
              }
            >
              A view reads from
            </KeyRow>
          </ul>
        </section>
        {diff && (
          <section className="grid gap-2">
            <h3 className="text-xs font-medium text-muted-foreground">Changes</h3>
            <ul className="grid gap-1.5">
              {(["added", "removed", "changed"] as const).map((s) => (
                <KeyRow
                  key={s}
                  icon={
                    <span className={cn("font-mono font-semibold", STATUS_TEXT[s])}>
                      {statusGlyph(s)}
                    </span>
                  }
                >
                  {s === "added"
                    ? "Added"
                    : s === "removed"
                      ? "Removed (only in the old version)"
                      : "Changed"}
                </KeyRow>
              ))}
            </ul>
          </section>
        )}
        <section className="grid gap-2">
          <h3 className="text-xs font-medium text-muted-foreground">Keyboard, on the graph</h3>
          <ul className="grid gap-1.5">
            <KeyRow icon={<KbdGroup keys={[modKey, "K"]} />}>Find a table (or /)</KeyRow>
            <KeyRow icon={<KbdGroup keys={["←", "→"]} />}>Move to the next table</KeyRow>
            <KeyRow icon={<Kbd>↵</Kbd>}>Zoom to the selected table</KeyRow>
            <KeyRow icon={<Kbd>Esc</Kbd>}>Clear the selection</KeyRow>
            <KeyRow icon={<KbdGroup keys={["+", "−", "0"]} />}>Zoom in, out, fit</KeyRow>
          </ul>
        </section>
      </PopoverContent>
    </Popover>
  );
}
