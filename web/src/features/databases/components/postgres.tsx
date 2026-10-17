import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import type * as React from "react";

import { WarningTriangleIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge, badgeVariants } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";

import {
  compiledOn,
  isMajor,
  majorDescription,
  MAJORS,
  majorLabel,
  type PgMajor,
  postgresMajorsQuery,
  pushedFromAnotherMajor,
  shortVersion,
} from "../postgres";
import type { Database, VersionSummary } from "../queries";

interface MajorSelectProps extends Omit<
  React.ComponentProps<typeof SelectTrigger>,
  "onChange" | "value"
> {
  value: PgMajor;
  onValueChange: (major: PgMajor) => void;
  /** The current major, marked as such (settings). */
  current?: PgMajor;
}

/**
 * Picks a Postgres major, newest first, saying under each whether this
 * server compiles it. One this server has no sandbox for cannot be picked;
 * one whose compiler is down can (it comes back).
 */
export function MajorSelect({
  value,
  onValueChange,
  current,
  className,
  ...trigger
}: MajorSelectProps) {
  const majors = useQuery(postgresMajorsQuery());
  const served = (major: PgMajor) => majors.data?.find((m) => m.major === major);
  return (
    <Select
      value={String(value)}
      onValueChange={(picked) => {
        const major = Number(picked);
        if (isMajor(major)) onValueChange(major);
      }}
    >
      <SelectTrigger className={cn("w-full sm:w-64", className)} {...trigger}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent className="sm:w-80">
        {MAJORS.map((major) => (
          <SelectItem
            key={major}
            value={String(major)}
            description={
              major === current
                ? ["Current", majorDescription(major, served(major))].filter(Boolean).join(" · ")
                : majorDescription(major, served(major)) || undefined
            }
            disabled={served(major)?.status === "off" && major !== value}
          >
            {majorLabel(major)}
          </SelectItem>
        ))}
      </SelectContent>
    </Select>
  );
}

/**
 * The database's major, in its header: quiet, as metadata is. Writers can
 * follow it to the settings that change it.
 */
export function DatabaseMajorBadge({
  org,
  database,
  editable,
}: {
  org: string;
  database: Database;
  editable: boolean;
}) {
  const label = majorLabel(database.pg_major);
  const title = `Its SQL compiles on ${label}, and migrations are written for it.`;
  if (!editable) {
    return (
      <Badge variant="outline" title={title}>
        {label}
      </Badge>
    );
  }
  return (
    <Link
      to="/$org/$db/settings"
      params={{ org, db: database.slug }}
      title={`${title} Change it in Settings.`}
      aria-label={`${label}: change it in the database's settings`}
      className={cn(
        badgeVariants({ variant: "outline" }),
        "outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
      )}
    >
      {label}
    </Link>
  );
}

/**
 * What a version was compiled on, or pushed from. A push from another
 * major than its database targeted is flagged (a warning, with a word).
 */
export function VersionPostgresBadge({
  version,
}: {
  version: Pick<VersionSummary, "origin" | "postgres">;
}) {
  const { postgres } = version;
  if (postgres.major == null) return null;
  const pushed = version.origin === "push";
  const said = compiledOn(postgres, pushed);
  if (postgres.other_major) {
    return (
      <Badge
        variant="warning"
        title={`${said}, while the database targeted ${majorLabel(postgres.database_major)}.`}
      >
        <WarningTriangleIcon aria-hidden />
        <span className="sr-only">Pushed from another version:</span>
        {majorLabel(postgres.major)}
      </Badge>
    );
  }
  return (
    <Badge variant="outline" title={said}>
      {majorLabel(postgres.major)}
    </Badge>
  );
}

/**
 * Above the history when the latest version was pushed from a server on
 * another major: the real database moved, and romulus can follow it.
 * Writers get the switch (the settings' dry run, preselected).
 */
export function OtherMajorAlert({
  org,
  database,
  writer,
}: {
  org: string;
  database: Database;
  writer: boolean;
}) {
  const pushed = pushedFromAnotherMajor(database);
  if (!pushed) return null;
  const from = shortVersion(pushed.postgres.server_version) ?? String(pushed.major);
  const link = (className: string) => (
    <Button size="sm" asChild className={className}>
      <Link
        to="/$org/$db/settings"
        params={{ org, db: database.slug }}
        search={{ major: pushed.major }}
      >
        Switch to {majorLabel(pushed.major)}…
      </Link>
    </Button>
  );
  return (
    <Alert
      variant="warning"
      title={`v${pushed.number} was pushed from Postgres ${from}`}
      // Beside the text on wide screens, under it on phones (one or the other is shown).
      action={writer && link("max-sm:hidden")}
    >
      <p>
        This database is set to {majorLabel(database.pg_major)}: its drafts and migrations follow
        that, not the server CI pushed from.
        {writer ? "" : " A developer can switch it in Settings."}
      </p>
      {writer && link("mt-2 sm:hidden")}
    </Alert>
  );
}
