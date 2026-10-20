import type * as React from "react";

import { PageIcon } from "@/components/icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import type { VersionDetail } from "@/features/databases/queries";
import { CodeView } from "@/features/schema/text/code-view";
import { cn } from "@/lib/utils";

import { formatLabel, skipReasonHint, skipReasonLabel } from "../labels";

/**
 * An imported version's migration: the file it came from, its SQL as
 * replayed, and what of it was not replayed. Opened from the version strip.
 */
export function MigrationButton({
  version,
  test,
}: {
  version: VersionDetail;
  /** Test on data, above the migration. */
  test?: React.ReactNode;
}) {
  const migration = version.migration;
  if (!migration) return null;
  const skipped = migration.skipped;
  return (
    <Dialog>
      <DialogTrigger asChild>
        <Button variant="ghost" size="sm" className="shrink-0">
          <PageIcon />
          <span className="max-lg:sr-only">Migration</span>
          {skipped.length > 0 && (
            <Badge variant="neutral" className="max-lg:hidden">
              {skipped.length} skipped
            </Badge>
          )}
        </Button>
      </DialogTrigger>
      <DialogContent
        className={cn(
          "top-[6vh] flex max-h-[88dvh] max-w-3xl flex-col gap-0 overflow-hidden p-0",
          "max-sm:top-0 max-sm:h-dvh max-sm:max-h-none max-sm:w-full max-sm:rounded-none",
        )}
      >
        <DialogHeader className="px-5 pt-5 pb-4">
          <DialogTitle className="font-mono text-base break-all">{version.message}</DialogTitle>
          <DialogDescription>
            v{version.number} is the schema after this {formatLabel(migration.format)} migration
            {migration.path === version.message ? (
              "."
            ) : (
              <>
                , imported from <span className="font-mono">{migration.path}</span>.
              </>
            )}
          </DialogDescription>
        </DialogHeader>
        {test && <div className="flex shrink-0 justify-end px-5 pb-3">{test}</div>}
        <div className="h-80 min-h-0 flex-1 border-y border-border">
          <CodeView
            doc={version.source}
            language="sql"
            label={`${migration.path}, as replayed`}
            readOnly
            active
          />
        </div>
        <section aria-label="Not replayed" className="max-h-56 shrink-0 overflow-y-auto px-5 py-3">
          {skipped.length === 0 ? (
            <p className="text-sm text-muted-foreground">Every statement was replayed.</p>
          ) : (
            <>
              <h3 className="mb-1.5 text-sm font-medium text-foreground">
                Not replayed ({skipped.length})
              </h3>
              <ul className="grid gap-1">
                {skipped.map((s, i) => (
                  <li key={i} className="flex min-w-0 items-center gap-2 text-sm">
                    <span className="w-12 shrink-0 font-mono text-xs text-faint-foreground tabular-nums">
                      L{s.line}
                    </span>
                    <span
                      className="min-w-0 flex-1 truncate font-mono text-xs text-foreground"
                      title={s.text}
                    >
                      {s.text}
                    </span>
                    <Badge variant="neutral" title={skipReasonHint[s.reason]}>
                      {skipReasonLabel[s.reason]}
                    </Badge>
                  </li>
                ))}
              </ul>
            </>
          )}
        </section>
      </DialogContent>
    </Dialog>
  );
}
