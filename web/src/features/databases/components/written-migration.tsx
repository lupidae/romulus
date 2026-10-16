import { useQuery } from "@tanstack/react-query";
import * as React from "react";

import { ApiError } from "@/api/client";
import { CheckIcon, CopyIcon, DatabaseScriptIcon, DownloadIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Skeleton } from "@/components/ui/skeleton";
import { toast } from "@/components/ui/toaster";
import { save } from "@/features/migration/zip";
import { CodeView } from "@/features/schema/text/code-view";
import { cn } from "@/lib/utils";

import { type Database, type VersionDetail, writtenMigrationQuery } from "../queries";

/**
 * A version published from a draft in migration mode: the migration it was
 * written as, to read again, copy or download. Opened from the version strip.
 */
export function WrittenMigrationButton({
  org,
  database,
  version,
  label,
}: {
  org: string;
  database: Database;
  version: VersionDetail;
  /** Always shown (by default "Migration", hidden on narrow screens). */
  label?: string;
}) {
  const [open, setOpen] = React.useState(false);
  if (!version.written_migration) return null;
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button variant="ghost" size="sm" className="shrink-0">
          <DatabaseScriptIcon />
          <span className={label ? undefined : "max-lg:sr-only"}>{label ?? "Migration"}</span>
        </Button>
      </DialogTrigger>
      <DialogContent
        className={cn(
          "top-[6vh] flex max-h-[88dvh] max-w-3xl flex-col gap-0 overflow-hidden p-0",
          "max-sm:top-0 max-sm:h-dvh max-sm:max-h-none max-sm:w-full max-sm:rounded-none",
        )}
      >
        <DialogHeader className="px-5 pt-5 pb-4">
          <DialogTitle>The migration of v{version.number}</DialogTitle>
          <DialogDescription>
            Written by hand in the draft that became v{version.number}
            {version.number > 1
              ? `: it turns v${version.number - 1} into v${version.number}.`
              : "."}
          </DialogDescription>
        </DialogHeader>
        {open && <Body org={org} database={database} number={version.number} />}
      </DialogContent>
    </Dialog>
  );
}

function Body({ org, database, number }: { org: string; database: Database; number: number }) {
  const migration = useQuery(writtenMigrationQuery(org, database.slug, database.id, number));
  const [copied, setCopied] = React.useState(false);
  React.useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 2000);
    return () => window.clearTimeout(timer);
  }, [copied]);
  const data = migration.data;

  if (migration.isError) {
    return (
      <div className="px-5 pb-5">
        <Alert
          variant="destructive"
          title="Could not load the migration"
          action={
            <Button size="sm" onClick={() => void migration.refetch()}>
              Retry
            </Button>
          }
        >
          {migration.error instanceof ApiError
            ? migration.error.message
            : "Check your connection and try again."}
        </Alert>
      </div>
    );
  }
  return (
    <>
      <div className="flex shrink-0 items-center gap-2 border-t border-border px-5 py-2">
        <span className="min-w-0 flex-1 truncate font-mono text-xs text-muted-foreground">
          {data?.file_name ?? <Skeleton className="h-3 w-40" />}
        </span>
        <Button
          size="sm"
          disabled={!data}
          aria-label={copied ? "Copied" : "Copy the migration"}
          onClick={() => {
            if (!data) return;
            void navigator.clipboard.writeText(data.sql).then(
              () => setCopied(true),
              () => toast.error("Could not copy", { description: "Select the text and copy it." }),
            );
          }}
        >
          {copied ? <CheckIcon className="text-success" /> : <CopyIcon />}
          {copied ? "Copied" : "Copy"}
        </Button>
        <Button
          size="sm"
          variant="primary"
          disabled={!data}
          onClick={() => data && save(data.file_name, data.sql)}
        >
          <DownloadIcon />
          Download
        </Button>
      </div>
      <div className="h-[60dvh] min-h-0 flex-1 border-t border-border max-sm:h-auto">
        {data ? (
          <CodeView
            doc={data.sql}
            language="sql"
            label={`The migration of v${number}`}
            readOnly
            active
          />
        ) : (
          <div aria-busy aria-label="Loading the migration" className="grid gap-2.5 px-5 py-3">
            {[70, 52, 84, 40].map((w, i) => (
              <Skeleton key={i} className="h-3" style={{ width: `${w}%` }} />
            ))}
          </div>
        )}
      </div>
    </>
  );
}
