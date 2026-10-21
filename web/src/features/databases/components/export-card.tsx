import { useMutation } from "@tanstack/react-query";
import type * as React from "react";

import { ApiError } from "@/api/client";
import { DownloadIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { toast } from "@/components/ui/toaster";
import { formatWait, retryAfter } from "@/features/auth/password";

import { databaseExportUrl, downloadExport, orgExportUrl } from "../export";

/**
 * Downloads the export at `href`: the session cookie goes along (same
 * origin), the API streams the ZIP and names the file. A refusal is a toast
 * (with the wait a 429 asks for), never a file.
 */
export function ExportButton({
  href,
  label,
  size = "md",
}: {
  href: string;
  label: string;
  size?: "sm" | "md";
}) {
  const download = useMutation({
    mutationFn: () => downloadExport(href),
    onError: (error) => {
      const wait = retryAfter(error);
      if (wait !== undefined) {
        toast.error("Too many exports for now", {
          description: `Try again in ${formatWait(wait)}.`,
        });
      } else {
        toast.error("The export could not be downloaded", {
          description: error instanceof ApiError ? error.message : "Check your connection.",
        });
      }
    },
  });
  return (
    <Button size={size} onClick={() => download.mutate()} loading={download.isPending}>
      <DownloadIcon />
      {label}
    </Button>
  );
}

const CONTENTS =
  "every version's SQL, the migration between each pair of consecutive versions (up and down, with the renames chosen), comments as JSON, and a manifest. Versions hidden by a plan are included.";

/** "Export database": everything about one database, as a ZIP. */
export function DatabaseExportCard({ org, db }: { org: string; db: string }) {
  return (
    <ExportCard
      title="Export"
      description={`A ZIP of this database: ${CONTENTS}`}
      action={<ExportButton href={databaseExportUrl(org, db)} label="Export database (ZIP)" />}
    />
  );
}

/** "Export organization": every database, as one ZIP (admins). */
export function OrgExportCard({ org }: { org: string }) {
  return (
    <ExportCard
      title="Export"
      description={`A ZIP with a folder per database: ${CONTENTS}`}
      action={<ExportButton href={orgExportUrl(org)} label="Export organization (ZIP)" />}
      className="max-w-2xl"
    />
  );
}

function ExportCard({
  title,
  description,
  action,
  className,
}: {
  title: string;
  description: string;
  action: React.ReactNode;
  className?: string;
}) {
  return (
    <Card className={className}>
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle>{title}</CardTitle>
          <p className="text-sm text-muted-foreground">{description}</p>
        </div>
      </CardHeader>
      <CardContent>{action}</CardContent>
    </Card>
  );
}
