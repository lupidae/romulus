import { Skeleton } from "@/components/ui/skeleton";

const CARDS = [
  { left: "10%", top: "12%", rows: 5 },
  { left: "42%", top: "8%", rows: 6 },
  { left: "40%", top: "54%", rows: 3 },
  { left: "70%", top: "26%", rows: 4 },
];

/** The time machine's shape while its history loads: the bar, tables, the timeline. */
export function TimeMachineSkeleton() {
  return (
    <div aria-busy className="flex h-full min-h-0 flex-col">
      <span className="sr-only" role="status">
        Loading the history
      </span>
      <div className="flex min-h-11 shrink-0 items-center gap-2 border-b border-border px-3 md:px-4">
        <Skeleton className="size-4 rounded-xs" />
        <Skeleton className="h-3.5 w-28" />
        <Skeleton className="h-3.5 w-20" />
        <Skeleton className="ml-auto hidden h-7 w-72 md:block" />
      </div>
      <div className="relative min-h-0 flex-1 overflow-hidden bg-background">
        {CARDS.map((c, i) => (
          <div
            key={i}
            className="absolute w-44 overflow-hidden rounded-lg border border-border bg-card shadow-raised"
            style={{ left: c.left, top: c.top }}
          >
            <div className="flex h-10 items-center gap-2 border-b border-border px-3">
              <Skeleton className="size-4 rounded-xs" />
              <Skeleton className="h-3 w-20" />
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
      <div className="shrink-0 border-t border-border bg-card px-3 py-2 md:px-4">
        {/* A phone's strip of the track, above the bar. */}
        <div className="mb-2 flex h-7 items-end gap-1 px-3.5 md:hidden">
          {Array.from({ length: 16 }, (_, i) => (
            <Skeleton key={i} className="flex-1" style={{ height: `${25 + ((i * 37) % 60)}%` }} />
          ))}
        </div>
        <div className="flex h-9 items-center gap-2">
          <Skeleton className="size-8 rounded-md" />
          <Skeleton className="size-8 rounded-md" />
          <Skeleton className="h-3.5 w-48" />
          <Skeleton className="ml-auto hidden h-8 w-52 rounded-lg md:block" />
        </div>
        <div className="hidden h-[5.75rem] items-end gap-1 pt-2 pb-8 md:flex">
          {Array.from({ length: 48 }, (_, i) => (
            <Skeleton key={i} className="flex-1" style={{ height: `${20 + ((i * 37) % 60)}%` }} />
          ))}
        </div>
      </div>
    </div>
  );
}
