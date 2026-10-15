import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import {
  CheckIcon,
  ClockIcon,
  NavArrowDownIcon,
  NavArrowLeftIcon,
  NavArrowRightIcon,
} from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuSeparator,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Tooltip } from "@/components/ui/tooltip";
import type { Groups, Layout } from "@/features/schema/layout";
import { parseSchema, SchemaParseError } from "@/features/schema/model";
import { SchemaViewer } from "@/features/schema/viewer/schema-viewer";
import { SchemaViewerSkeleton } from "@/features/schema/viewer/viewer-skeleton";
import { timeAgo } from "@/lib/time";
import { useShortcut } from "@/lib/use-shortcut";

import {
  type Database,
  originLabel,
  versionQuery,
  versionsQuery,
  versionTextsQuery,
  type VersionDetail,
} from "../queries";
import { Byline, OriginBadge } from "./version-meta";

interface VersionViewProps {
  org: string;
  database: Database;
  number: number;
}

/**
 * One version, read-only: a strip saying which one it is (with the way to
 * the others), then the schema viewer, graph and text.
 */
export function VersionView({ org, database, number }: VersionViewProps) {
  const db = database.slug;
  const version = useQuery(versionQuery(org, db, database.id, number));
  const texts = useQuery(versionTextsQuery(org, db, database.id, number));
  const latest = database.latest?.number ?? number;

  const model = React.useMemo(() => {
    if (!version.data) return undefined;
    try {
      return { schema: parseSchema(version.data.model), ...presentation(version.data.layout) };
    } catch (error) {
      return { error: error instanceof SchemaParseError ? error.message : String(error) };
    }
  }, [version.data]);

  const failed = version.error ?? texts.error;
  return (
    <div className="flex h-full min-h-0 flex-col">
      <VersionStrip
        org={org}
        database={database}
        number={number}
        latest={latest}
        version={version.data}
      />
      <div className="relative min-h-0 flex-1">
        {failed ? (
          <div className="p-4 md:p-6">
            <Alert
              variant="destructive"
              title={`Could not load v${number}`}
              action={
                <Button
                  size="sm"
                  onClick={() => {
                    void version.refetch();
                    void texts.refetch();
                  }}
                >
                  Retry
                </Button>
              }
            >
              {failed.message}
            </Alert>
          </div>
        ) : model && "error" in model ? (
          <div className="p-4 md:p-6">
            <Alert variant="destructive" title={`v${number} cannot be drawn`}>
              Its model is not one this viewer understands ({model.error}).
            </Alert>
          </div>
        ) : model && texts.data ? (
          <SchemaViewer
            schema={model.schema}
            texts={texts.data}
            layout={model.layout}
            groups={model.groups}
            readOnly
            storageKey="romulus:version-viewer"
          />
        ) : (
          <SchemaViewerSkeleton />
        )}
      </div>
    </div>
  );
}

/** A version's layout snapshot (`{layout, groups}`, as drafts publish it), leniently. */
function presentation(value: unknown): { layout?: Layout; groups?: Groups } {
  if (typeof value !== "object" || value === null) return {};
  const { layout, groups } = value as { layout?: unknown; groups?: unknown };
  const isMap = (v: unknown) => typeof v === "object" && v !== null && !Array.isArray(v);
  return {
    layout: isMap(layout) ? (layout as Layout) : undefined,
    groups: isMap(groups) ? (groups as Groups) : undefined,
  };
}

function VersionStrip({
  org,
  database,
  number,
  latest,
  version,
}: {
  org: string;
  database: Database;
  number: number;
  latest: number;
  version?: VersionDetail;
}) {
  const navigate = useNavigate();
  const db = database.slug;
  const go = (n: number) =>
    n === latest
      ? navigate({ to: "/$org/$db", params: { org, db } })
      : navigate({ to: "/$org/$db/v/$number", params: { org, db, number: n } });
  const older = number > 1 ? number - 1 : undefined;
  const newer = number < latest ? number + 1 : undefined;
  useShortcut("[", () => older && void go(older), !!older);
  useShortcut("]", () => newer && void go(newer), !!newer);

  return (
    <div className="flex min-h-11 shrink-0 flex-wrap items-center gap-x-3 gap-y-1 border-b border-border px-3 py-1.5 md:px-4">
      <div className="flex items-center gap-0.5">
        <Tooltip content="Older version" shortcut={["["]}>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Older version"
            disabled={!older}
            onClick={() => older && void go(older)}
          >
            <NavArrowLeftIcon />
          </Button>
        </Tooltip>
        <VersionPicker org={org} database={database} number={number} latest={latest} />
        <Tooltip content="Newer version" shortcut={["]"]}>
          <Button
            variant="ghost"
            size="icon-sm"
            aria-label="Newer version"
            disabled={!newer}
            onClick={() => newer && void go(newer)}
          >
            <NavArrowRightIcon />
          </Button>
        </Tooltip>
      </div>
      {number === latest ? (
        <Badge variant="success">
          <CheckIcon aria-hidden />
          Latest
        </Badge>
      ) : (
        <span className="flex items-center gap-2">
          <Badge variant="warning">
            <ClockIcon aria-hidden />
            Older version
          </Badge>
          <Button variant="link" size="sm" asChild>
            <Link to="/$org/$db" params={{ org, db }}>
              Go to v{latest}
            </Link>
          </Button>
        </span>
      )}
      {version ? (
        <div className="hidden min-w-0 flex-1 items-center gap-3 text-sm md:flex">
          <OriginBadge origin={version.origin} />
          <Byline author={version.author} at={version.created_at} />
          {version.message && (
            <span
              className="hidden min-w-0 truncate text-foreground lg:inline"
              title={version.message}
            >
              {version.message}
            </span>
          )}
        </div>
      ) : (
        <span className="flex-1" />
      )}
    </div>
  );
}

/** "v12 ▾": the recent versions, one click away. */
function VersionPicker({
  org,
  database,
  number,
  latest,
}: {
  org: string;
  database: Database;
  number: number;
  latest: number;
}) {
  const [open, setOpen] = React.useState(false);
  const versions = useInfiniteQuery({ ...versionsQuery(org, database.slug), enabled: open });
  const recent = versions.data?.pages[0]?.versions.slice(0, 12) ?? [];
  const db = database.slug;
  return (
    <Menu open={open} onOpenChange={setOpen}>
      <MenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          className="font-mono text-sm font-semibold text-foreground"
        >
          v{number}
          <NavArrowDownIcon className="text-muted-foreground" />
          <span className="sr-only">: choose another version</span>
        </Button>
      </MenuTrigger>
      <MenuContent align="start" className="w-80">
        <MenuLabel>Recent versions</MenuLabel>
        {versions.isPending
          ? Array.from({ length: Math.min(latest, 5) }, (_, i) => (
              <div key={i} className="mx-1 h-8 animate-pulse rounded-md bg-muted/60" />
            ))
          : recent.map((v) => (
              <MenuItem key={v.id} asChild>
                <Link
                  to={v.number === latest ? "/$org/$db" : "/$org/$db/v/$number"}
                  params={{ org, db, number: v.number }}
                  className="flex items-center gap-2"
                >
                  <span className="w-9 shrink-0 font-mono text-sm font-medium text-foreground tabular-nums">
                    v{v.number}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-sm text-muted-foreground">
                    {v.message ??
                      `${originLabel[v.origin]} by ${v.author?.name ?? "a deleted account"}`}
                  </span>
                  <span className="shrink-0 text-xs text-faint-foreground">
                    {timeAgo(v.created_at)}
                  </span>
                  {v.number === number && <CheckIcon aria-label="Shown" className="text-primary" />}
                </Link>
              </MenuItem>
            ))}
        <MenuSeparator />
        <MenuItem asChild>
          <Link to="/$org/$db/history" params={{ org, db }}>
            <ClockIcon />
            All {latest} versions
          </Link>
        </MenuItem>
      </MenuContent>
    </Menu>
  );
}
