import { createFileRoute } from "@tanstack/react-router";

import { PlusIcon } from "@/components/icons";
import { PageHeader } from "@/components/shell/page-header";
import { Avatar } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { toast } from "@/components/ui/toaster";

import { people } from "../-fixtures";

export const Route = createFileRoute("/dev/shell/members")({
  head: () => ({ meta: [{ title: "Members – romulus" }] }),
  component: Members,
});

function Members() {
  return (
    <>
      <PageHeader
        title="Members"
        description="Who can read, edit and manage this organization's databases."
        actions={
          <Button variant="primary" onClick={() => toast.success("Invitation link copied")}>
            <PlusIcon />
            Invite
          </Button>
        }
      />
      <Card>
        <ul className="divide-y divide-border">
          {people.map((p) => (
            <li key={p.id} className="flex h-14 items-center gap-3 px-4">
              <Avatar id={p.id} name={p.name} size="lg" />
              <div className="grid min-w-0 flex-1">
                <span className="truncate text-sm font-medium text-foreground">{p.name}</span>
                <span className="truncate text-sm text-muted-foreground">{p.email}</span>
              </div>
              <Badge variant={p.role === "admin" ? "primary" : "neutral"} className="capitalize">
                {p.role}
              </Badge>
            </li>
          ))}
        </ul>
      </Card>
    </>
  );
}
