import { Skeleton } from "@/components/ui/skeleton";
import { SchemaViewerSkeleton } from "@/features/schema/viewer/viewer-skeleton";
import { cn } from "@/lib/utils";

/** The diff's shape while it loads: the change list, then the graph and the SQL. */
export function DiffSkeleton({ className }: { className?: string }) {
  return (
    <div aria-busy className={cn("flex h-full min-h-0 w-full bg-card", className)}>
      <span className="sr-only" role="status">
        Comparing the versions
      </span>
      <div className="hidden w-72 shrink-0 flex-col border-r border-border lg:flex xl:w-80">
        <div className="grid gap-2.5 border-b border-border p-3">
          <Skeleton className="h-4 w-24" />
          <Skeleton className="h-3 w-48" />
          <Skeleton className="h-8 w-full rounded-lg" />
        </div>
        <div className="grid content-start gap-4 px-3 py-4">
          {[3, 1, 2, 1, 2].map((lines, i) => (
            <div key={i} className="grid grid-cols-[1rem_1fr] gap-x-2 gap-y-1.5">
              <Skeleton className="size-4 rounded-xs" />
              <Skeleton className="h-3.5 w-28" />
              {Array.from({ length: lines }, (_, l) => (
                <Skeleton
                  key={l}
                  className="col-start-2 h-2.5"
                  style={{ width: `${82 - l * 18}%` }}
                />
              ))}
            </div>
          ))}
        </div>
      </div>
      <SchemaViewerSkeleton className="min-w-0 flex-1" />
    </div>
  );
}
