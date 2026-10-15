import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

const CARDS = [
  { left: "8%", top: "14%", rows: 5 },
  { left: "40%", top: "8%", rows: 7 },
  { left: "38%", top: "58%", rows: 4 },
  { left: "8%", top: "56%", rows: 3 },
];

/** The viewer's shape while its data loads: tables on a canvas, lines of code. */
export function SchemaViewerSkeleton({ className }: { className?: string }) {
  return (
    <div aria-busy className={cn("flex h-full min-h-0 w-full bg-card", className)}>
      <span className="sr-only" role="status">
        Loading the schema
      </span>
      <div className="relative min-w-0 flex-[3] overflow-hidden bg-background">
        {CARDS.map((c, i) => (
          <div
            key={i}
            className="absolute w-48 overflow-hidden rounded-lg border border-border bg-card shadow-raised"
            style={{ left: c.left, top: c.top }}
          >
            <div className="flex h-10 items-center gap-2 border-b border-border px-3">
              <Skeleton className="size-4 rounded-xs" />
              <Skeleton className="h-3 w-24" />
            </div>
            <div className="grid gap-3 px-3 py-3">
              {Array.from({ length: c.rows }, (_, r) => (
                <div key={r} className="flex justify-between gap-6">
                  <Skeleton className="h-2.5 w-16" />
                  <Skeleton className="h-2.5 w-10" />
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
      <div className="hidden min-w-0 flex-[2] flex-col border-l border-border md:flex">
        <div className="flex h-11 items-center gap-1 border-b border-border px-2">
          <Skeleton className="h-6.5 w-56 rounded-lg" />
        </div>
        <div className="grid content-start gap-2.5 px-4 py-3">
          {[60, 84, 72, 44, 80, 52, 90, 38, 66, 74, 48].map((w, i) => (
            <Skeleton key={i} className="h-3" style={{ width: `${w}%` }} />
          ))}
        </div>
      </div>
    </div>
  );
}
