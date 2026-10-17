import { Link } from "@tanstack/react-router";

import { CloudIcon, ImportIcon, KeyIcon, PageIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { CodeBlock } from "@/components/ui/code-block";
import { Kbd } from "@/components/ui/kbd";

import { pushCommand } from "../push-command";
import type { Database } from "../queries";

/**
 * A database without a version yet: the ways in. Upload SQL by hand, import
 * a folder of migrations as the history, or push from CI with remus (the
 * real command, this database's id in it). Reviewers read what will happen,
 * without the buttons.
 */
export function GetStarted({
  org,
  database,
  writer,
  onUpload,
}: {
  org: string;
  database: Database;
  writer: boolean;
  onUpload: () => void;
}) {
  return (
    <div className="mx-auto grid w-full max-w-5xl gap-6 px-4 py-8 md:px-8 md:py-10">
      <div className="grid gap-1">
        <h2 className="text-lg font-semibold text-foreground">
          {/* Every version removed: the settings list them for 30 days. */}
          {database.next_number > 1 ? "No versions left" : "No versions yet"}
        </h2>
        <p className="max-w-prose text-base text-muted-foreground">
          {writer
            ? "Bring in the first one: upload the schema's SQL, import your migrations as the history, or push it from CI."
            : "The first one appears here once a developer uploads SQL, imports migrations or CI pushes it."}
        </p>
      </div>
      <div className="grid gap-4 lg:grid-cols-2">
        <Card className="flex flex-col gap-4 p-5">
          <Way icon={<PageIcon />} title="Upload SQL">
            Paste your DDL or drop a .sql file. It is compiled on Postgres {database.pg_major} in a
            sandbox and saved as v1 if it compiles.
          </Way>
          {writer && (
            <div className="mt-auto">
              <Button variant="primary" onClick={onUpload}>
                Upload SQL
                <Kbd className="ml-1 border-primary-foreground/25 bg-primary-foreground/10 text-primary-foreground shadow-none">
                  U
                </Kbd>
              </Button>
            </div>
          )}
        </Card>
        <Card className="flex flex-col gap-4 p-5">
          <Way icon={<ImportIcon />} title="Import migrations">
            Drop your migrations folder (sqlx, Flyway, Prisma, dbmate…): each migration becomes a
            version, so the history starts where your project did.
          </Way>
          {writer && (
            <div className="mt-auto">
              <Button asChild>
                <Link to="/$org/$db/import" params={{ org, db: database.slug }}>
                  Import migrations
                  <Kbd className="ml-1">I</Kbd>
                </Link>
              </Button>
            </div>
          )}
        </Card>
        <Card className="flex min-w-0 flex-col gap-4 p-5 lg:col-span-2">
          <Way icon={<CloudIcon />} title="Push from CI">
            remus sends the live database&apos;s schema. Unchanged schemas are skipped, so run it on
            every build.
          </Way>
          <CodeBlock code={pushCommand(database.id)} label="Push command" />
          {writer && (
            <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-muted-foreground">
              <KeyIcon className="size-3.5" />
              Set ROMULUS_TOKEN to an API token of this database.
              <Link
                to="/$org/$db/settings"
                params={{ org, db: database.slug }}
                hash="tokens"
                className="font-medium text-primary underline-offset-4 hover:underline"
              >
                Create a token
              </Link>
            </p>
          )}
        </Card>
      </div>
    </div>
  );
}

function Way({
  icon,
  title,
  children,
}: {
  icon: React.ReactNode;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div className="flex items-start gap-3">
      <span className="flex size-8 shrink-0 items-center justify-center rounded-md border border-border bg-muted text-muted-foreground [&_svg]:size-4">
        {icon}
      </span>
      <div className="grid gap-1">
        <h3 className="text-base font-semibold text-foreground">{title}</h3>
        <p className="text-sm text-muted-foreground">{children}</p>
      </div>
    </div>
  );
}
