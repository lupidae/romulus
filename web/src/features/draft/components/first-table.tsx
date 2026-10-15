import * as React from "react";

import { TableIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import { EmptyState } from "@/components/ui/empty-state";
import { Input } from "@/components/ui/input";
import { nameProblem } from "@/features/schema/edits/quote";

/**
 * An empty draft's graph, for someone who may edit: name the first table
 * right here (it starts with an id column), or write SQL.
 */
export function FirstTable({ onCreate }: { onCreate: (name: string) => void }) {
  const [name, setName] = React.useState("");
  const [tried, setTried] = React.useState(false);
  const problem = nameProblem(name.trim());
  return (
    <EmptyState
      icon={<TableIcon />}
      title="No tables yet"
      description="Name the first table, or write CREATE TABLE in the SQL: tables appear here as you type."
      action={
        <form
          className="grid w-72 gap-1.5 text-left"
          onSubmit={(event) => {
            event.preventDefault();
            setTried(true);
            if (!problem) onCreate(name.trim());
          }}
        >
          <div className="flex gap-2">
            <Input
              aria-label="Table name"
              aria-invalid={tried && problem ? true : undefined}
              aria-describedby={tried && problem ? "first-table-problem" : undefined}
              value={name}
              onChange={(event) => setName(event.target.value)}
              placeholder="users"
              spellCheck={false}
              autoComplete="off"
              className="font-mono"
            />
            <Button type="submit" variant="primary" className="shrink-0">
              Add table
            </Button>
          </div>
          {tried && problem && (
            <p id="first-table-problem" className="text-sm text-destructive">
              {problem}
            </p>
          )}
        </form>
      }
    />
  );
}
