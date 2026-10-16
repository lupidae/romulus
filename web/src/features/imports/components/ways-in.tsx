import { Link, useParams } from "@tanstack/react-router";
import * as React from "react";

import { CloudIcon, KeyIcon, TerminalIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { CodeBlock } from "@/components/ui/code-block";
import { ToggleGroup, ToggleGroupItem } from "@/components/ui/toggle-group";

import { exporters, importCommand } from "../commands";

/**
 * The other ways in: migrations written as code (Alembic, Django, Rails)
 * through an exporter script, and imports from CI.
 */
export function WaysIn({ databaseId }: { databaseId: string }) {
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <Exporters />
      <FromCi databaseId={databaseId} />
    </div>
  );
}

function Exporters() {
  const [tool, setTool] = React.useState<(typeof exporters)[number]["tool"]>("alembic");
  const chosen = exporters.find((e) => e.tool === tool) ?? exporters[0];
  const download = () => {
    const url = URL.createObjectURL(new Blob([chosen.script], { type: "text/x-shellscript" }));
    const link = document.createElement("a");
    link.href = url;
    link.download = chosen.file;
    link.click();
    URL.revokeObjectURL(url);
  };
  return (
    <Card className="flex min-w-0 flex-col gap-3 p-4">
      <Heading icon={<TerminalIcon />} title="Migrations written in code">
        romulus never runs your code. Run the exporter in your project: it writes the SQL each
        migration runs, then drop that file here.
      </Heading>
      <ToggleGroup
        type="single"
        value={tool}
        onValueChange={(value) => value && setTool(value as typeof tool)}
        aria-label="Tool"
        className="self-start"
      >
        {exporters.map((e) => (
          <ToggleGroupItem key={e.tool} value={e.tool}>
            {e.label}
          </ToggleGroupItem>
        ))}
      </ToggleGroup>
      <p className="text-sm text-muted-foreground">{chosen.note}</p>
      <CodeBlock code={chosen.run} label={`Export ${chosen.label} migrations`} />
      <div>
        <Button size="sm" onClick={download}>
          Download {chosen.file}
        </Button>
      </div>
    </Card>
  );
}

function FromCi({ databaseId }: { databaseId: string }) {
  const { org, db } = useParams({ from: "/$org/$db" });
  return (
    <Card className="flex min-w-0 flex-col gap-3 p-4">
      <Heading icon={<CloudIcon />} title="From CI">
        Import on every deploy: migrations already imported are skipped, new ones are added.
      </Heading>
      <CodeBlock code={importCommand(databaseId)} label="Import command" />
      <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm text-muted-foreground">
        <KeyIcon className="size-3.5" />
        Set ROMULUS_TOKEN to an API token of this database.
        <Link
          to="/$org/$db/settings"
          params={{ org, db }}
          hash="tokens"
          className="font-medium text-primary underline-offset-4 hover:underline"
        >
          Create a token
        </Link>
      </p>
    </Card>
  );
}

function Heading({
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
