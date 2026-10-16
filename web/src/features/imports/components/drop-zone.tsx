import * as React from "react";

import { ArchiveIcon, FolderIcon, ImportIcon } from "@/components/icons";
import { FileButton } from "@/components/ui/file-button";
import { Spinner } from "@/components/ui/spinner";
import { cn } from "@/lib/utils";

import { fromDrop, fromInput, type Picked } from "../files";

/**
 * Where an import starts: drop a migrations folder or a zip, or pick one.
 * The buttons are the keyboard's way in; the zone is a bigger target for a
 * drag.
 */
export function DropZone({
  busy,
  onPicked,
}: {
  /** Reading what was dropped, or planning it. */
  busy: boolean;
  onPicked: (picked: Picked) => void;
}) {
  const [over, setOver] = React.useState(false);
  return (
    <div
      onDragOver={(event) => {
        if (!event.dataTransfer.types.includes("Files")) return;
        event.preventDefault();
        event.dataTransfer.dropEffect = "copy";
        setOver(true);
      }}
      onDragLeave={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setOver(false);
      }}
      onDrop={(event) => {
        event.preventDefault();
        setOver(false);
        void fromDrop(event.dataTransfer).then(onPicked);
      }}
      className={cn(
        "relative flex min-h-64 flex-col items-center justify-center gap-4 rounded-lg border-2 border-dashed px-6 py-10 text-center transition-colors",
        over ? "border-primary bg-primary/5" : "border-border-strong bg-card",
      )}
    >
      <span className="flex size-11 items-center justify-center rounded-lg border border-border bg-muted text-muted-foreground [&_svg]:size-5">
        {busy ? (
          <Spinner className="size-5" label="Reading the files" />
        ) : (
          <ImportIcon aria-hidden />
        )}
      </span>
      <div className="grid max-w-md gap-1">
        <p className="text-base font-semibold text-foreground">
          {over ? "Drop to read the migrations" : "Drop a migrations folder or a .zip"}
        </p>
        <p className="text-sm text-muted-foreground">
          sqlx, Flyway, Prisma, dbmate, golang-migrate, node-pg-migrate or numbered .sql files. The
          format is detected; nothing is imported before you confirm.
        </p>
      </div>
      <div className="flex flex-wrap items-center justify-center gap-2">
        <FileButton
          variant="primary"
          directory
          disabled={busy}
          onFiles={(files) => onPicked(fromInput(files))}
        >
          <FolderIcon />
          Choose folder
        </FileButton>
        <FileButton
          multiple
          accept=".zip,.sql,application/zip,application/sql"
          disabled={busy}
          onFiles={(files) => onPicked(fromInput(files))}
        >
          <ArchiveIcon />
          Choose .zip or files
        </FileButton>
      </div>
    </div>
  );
}
