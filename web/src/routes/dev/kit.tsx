import { createFileRoute, Link, notFound } from "@tanstack/react-router";
import * as React from "react";

import { LogoMark, Wordmark } from "@/components/brand/logo";
import {
  CodeIcon,
  ComputerIcon,
  CopyIcon,
  DatabaseIcon,
  EditPencilIcon,
  GitBranchIcon,
  HalfMoonIcon,
  MoreHorizIcon,
  PlusIcon,
  SunLightIcon,
  TrashIcon,
} from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Avatar, AvatarStack } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { ConfirmDialog } from "@/components/ui/confirm-dialog";
import { CopyField } from "@/components/ui/copy-field";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import {
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuSeparator,
  MenuShortcut,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { EmptyState } from "@/components/ui/empty-state";
import { Field } from "@/components/ui/field";
import { Input, Textarea } from "@/components/ui/input";
import { Kbd, KbdGroup } from "@/components/ui/kbd";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { Skeleton } from "@/components/ui/skeleton";
import { Spinner } from "@/components/ui/spinner";
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { toast } from "@/components/ui/toaster";
import { Popover, PopoverContent, PopoverTrigger } from "@/components/ui/popover";
import { Tooltip } from "@/components/ui/tooltip";
import { modKey } from "@/lib/platform";
import { type ThemePreference, useTheme } from "@/lib/theme";
import { devRoutes } from "@/lib/dev-routes";

// Every token and kit component on one page, in both themes, for the people
// and agents building screens and for QA. Dev builds only.

export const Route = createFileRoute("/dev/kit")({
  beforeLoad: () => {
    if (!devRoutes) throw notFound();
  },
  head: () => ({ meta: [{ title: "Kit – romulus" }] }),
  component: Kit,
});

const sections = [
  ["colors", "Colors"],
  ["type", "Type"],
  ["shape", "Radius and depth"],
  ["buttons", "Button"],
  ["forms", "Input, Select, Field"],
  ["badges", "Badge, Kbd, Tooltip"],
  ["avatars", "Avatar"],
  ["tabs", "Tabs"],
  ["overlays", "Menu, Popover, Dialog, Toast"],
  ["feedback", "Alert, Spinner, Skeleton"],
  ["empty", "Empty state"],
  ["table", "Table, CopyField, ConfirmDialog"],
  ["shell", "App shell"],
] as const;

function Kit() {
  const { preference, setPreference } = useTheme();
  return (
    <div className="min-h-dvh bg-background">
      <header className="sticky top-0 z-(--z-shell) flex h-12 items-center gap-3 border-b border-border bg-background/90 px-4 backdrop-blur-sm">
        <Wordmark />
        <span className="text-sm text-muted-foreground">Kit</span>
        <div className="ml-auto">
          <Tabs value={preference} onValueChange={(v) => setPreference(v as ThemePreference)}>
            <TabsList aria-label="Theme">
              <TabsTrigger value="system" aria-label="System theme">
                <ComputerIcon />
                <span className="hidden sm:inline">System</span>
              </TabsTrigger>
              <TabsTrigger value="light" aria-label="Light theme">
                <SunLightIcon />
                <span className="hidden sm:inline">Light</span>
              </TabsTrigger>
              <TabsTrigger value="dark" aria-label="Dark theme">
                <HalfMoonIcon />
                <span className="hidden sm:inline">Dark</span>
              </TabsTrigger>
            </TabsList>
          </Tabs>
        </div>
      </header>

      <div className="mx-auto flex max-w-6xl gap-10 px-4 py-8 md:px-8">
        <nav aria-label="Sections" className="sticky top-20 hidden h-fit w-44 shrink-0 lg:block">
          <ul className="grid gap-px">
            {sections.map(([id, label]) => (
              <li key={id}>
                <a
                  href={`#${id}`}
                  className="flex h-7 items-center rounded-md px-2 text-sm text-muted-foreground outline-none hover:bg-accent hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
                >
                  {label}
                </a>
              </li>
            ))}
          </ul>
        </nav>

        <main className="grid min-w-0 flex-1 grid-cols-1 gap-12">
          <div className="grid gap-1">
            <h1 className="text-xl font-semibold">Component kit</h1>
            <p className="text-base text-muted-foreground">
              Tokens live in <code className="text-sm">src/styles/globals.css</code>, components in{" "}
              <code className="text-sm">src/components/ui</code>. Build screens from these only.
            </p>
          </div>
          <Colors />
          <TypeScale />
          <Shape />
          <Buttons />
          <Forms />
          <Badges />
          <Avatars />
          <TabsDemo />
          <Overlays />
          <Feedback />
          <Empty />
          <TableAndFriends />
          <ShellLinks />
        </main>
      </div>
    </div>
  );
}

function Section({
  id,
  title,
  children,
}: {
  id: string;
  title: string;
  children: React.ReactNode;
}) {
  return (
    <section
      id={id}
      aria-labelledby={`${id}-title`}
      className="grid scroll-mt-20 grid-cols-1 gap-4"
    >
      <h2 id={`${id}-title`} className="text-lg font-semibold">
        {title}
      </h2>
      {children}
    </section>
  );
}

function Specimen({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div className="grid gap-2">
      <p className="text-sm text-muted-foreground">{label}</p>
      <div className="flex flex-wrap items-center gap-3">{children}</div>
    </div>
  );
}

const swatches: { group: string; tokens: [string, string][] }[] = [
  {
    group: "Surfaces",
    tokens: [
      ["background", "bg-background"],
      ["card", "bg-card"],
      ["popover", "bg-popover"],
      ["muted", "bg-muted"],
      ["accent", "bg-accent"],
      ["highlight", "bg-highlight"],
      ["sidebar", "bg-sidebar"],
      ["border", "bg-border"],
      ["border-strong", "bg-border-strong"],
    ],
  },
  {
    group: "Text",
    tokens: [
      ["foreground", "bg-foreground"],
      ["muted-foreground", "bg-muted-foreground"],
      ["faint-foreground", "bg-faint-foreground"],
    ],
  },
  {
    group: "Action and status",
    tokens: [
      ["primary", "bg-primary"],
      ["primary-hover", "bg-primary-hover"],
      ["destructive", "bg-destructive"],
      ["success", "bg-success"],
      ["warning", "bg-warning"],
      ["info", "bg-info"],
    ],
  },
  {
    group: "Presence (collaborators)",
    tokens: [
      ["presence-1", "bg-presence-1"],
      ["presence-2", "bg-presence-2"],
      ["presence-3", "bg-presence-3"],
      ["presence-4", "bg-presence-4"],
      ["presence-5", "bg-presence-5"],
      ["presence-6", "bg-presence-6"],
      ["presence-7", "bg-presence-7"],
      ["presence-8", "bg-presence-8"],
    ],
  },
];

function Colors() {
  return (
    <Section id="colors" title="Colors">
      <div className="grid gap-6">
        {swatches.map(({ group, tokens }) => (
          <div key={group} className="grid gap-2">
            <p className="text-sm text-muted-foreground">{group}</p>
            <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
              {tokens.map(([name, cls]) => (
                <div key={name} className="overflow-hidden rounded-lg border border-border bg-card">
                  <div className={`h-10 border-b border-border ${cls}`} />
                  <p className="truncate px-2.5 py-1.5 font-mono text-xs text-muted-foreground">
                    {name}
                  </p>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
    </Section>
  );
}

function TypeScale() {
  const steps: [string, string, string, string][] = [
    ["text-display", "48", "font-semibold", "404"],
    ["text-2xl", "26", "font-semibold", "Sign in to romulus"],
    ["text-xl", "20", "font-semibold", "Databases"],
    ["text-lg", "16", "font-semibold", "Version history"],
    ["text-base", "14", "font-normal", "Body text and form controls. The default."],
    ["text-sm", "13", "font-medium", "Menus, navigation, table rows, buttons"],
    ["text-xs", "12", "font-normal", "Badges, keyboard hints, meta"],
  ];
  return (
    <Section id="type" title="Type">
      <Card className="divide-y divide-border">
        {steps.map(([cls, px, weight, sample]) => (
          <div key={cls} className="flex items-baseline gap-4 px-4 py-3">
            <span className="w-36 shrink-0 font-mono text-xs text-faint-foreground">
              {cls} · {px}
            </span>
            <span className={`${cls} ${weight} min-w-0 truncate`}>{sample}</span>
          </div>
        ))}
        <div className="flex items-baseline gap-4 px-4 py-3">
          <span className="w-36 shrink-0 font-mono text-xs text-faint-foreground">font-mono</span>
          <code className="truncate text-sm">
            ALTER TABLE users ADD COLUMN email_verified boolean;
          </code>
        </div>
      </Card>
    </Section>
  );
}

function Shape() {
  return (
    <Section id="shape" title="Radius and depth">
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-5">
        {[
          ["rounded-xs", "3 · kbd"],
          ["rounded-sm", "4 · badge"],
          ["rounded-md", "6 · control"],
          ["rounded-lg", "8 · panel"],
          ["rounded-xl", "12 · dialog"],
        ].map(([cls, label]) => (
          <div key={cls} className="grid gap-2">
            <div className={`h-14 border border-border-strong bg-card ${cls}`} />
            <p className="font-mono text-xs text-muted-foreground">{label}</p>
          </div>
        ))}
      </div>
      <div className="grid grid-cols-2 gap-4 sm:grid-cols-4">
        {[
          ["shadow-xs", "resting"],
          ["shadow-raised", "raised"],
          ["shadow-popover", "menu, popover"],
          ["shadow-dialog", "dialog"],
        ].map(([cls, label]) => (
          <div key={cls} className="grid gap-2">
            <div className={`h-16 rounded-lg bg-card ${cls}`} />
            <p className="font-mono text-xs text-muted-foreground">
              {cls} · {label}
            </p>
          </div>
        ))}
      </div>
    </Section>
  );
}

function Buttons() {
  return (
    <Section id="buttons" title="Button">
      <Card className="grid gap-6 p-4">
        <Specimen label="Variants">
          <Button variant="primary">
            <PlusIcon />
            New draft
          </Button>
          <Button>Compare versions</Button>
          <Button variant="ghost">Cancel</Button>
          <Button variant="destructive">
            <TrashIcon />
            Delete database
          </Button>
          <Button variant="link">View history</Button>
        </Specimen>
        <Specimen label="Sizes">
          <Button variant="primary" size="sm">
            Small
          </Button>
          <Button variant="primary">Medium</Button>
          <Button variant="primary" size="lg">
            Large
          </Button>
          <Tooltip content="Copy link" shortcut={[modKey, "C"]}>
            <Button size="icon" aria-label="Copy link">
              <CopyIcon />
            </Button>
          </Tooltip>
          <Tooltip content="More actions">
            <Button variant="ghost" size="icon-sm" aria-label="More actions">
              <MoreHorizIcon />
            </Button>
          </Tooltip>
        </Specimen>
        <Specimen label="States">
          <Button variant="primary" loading>
            Publish v13
          </Button>
          <Button loading>Saving</Button>
          <Button variant="primary" disabled>
            Disabled
          </Button>
        </Specimen>
      </Card>
    </Section>
  );
}

function Forms() {
  return (
    <Section id="forms" title="Input, Select, Field">
      <Card className="grid gap-5 p-4 sm:grid-cols-2">
        <Field label="Database name" description="Lowercase, used in links.">
          <Input placeholder="billing" />
        </Field>
        <Field label="Email" error="Enter an address like name@company.com.">
          <Input type="email" defaultValue="alice@" />
        </Field>
        <Field label="Role">
          <Select defaultValue="developer">
            <SelectTrigger>
              <SelectValue />
            </SelectTrigger>
            <SelectContent className="w-72">
              <SelectItem value="admin" description="Everything, plus members and settings">
                Admin
              </SelectItem>
              <SelectItem value="developer" description="Edits drafts, creates versions">
                Developer
              </SelectItem>
              <SelectItem value="reviewer" description="Reads everything, edits nothing">
                Reviewer
              </SelectItem>
            </SelectContent>
          </Select>
        </Field>
        <Field label="URL" description="A prefix is part of the control, not the value.">
          <Input prefix="romulus.app/app/" defaultValue="acme" className="font-mono" />
        </Field>
        <Field label="Disabled">
          <Input disabled defaultValue="read-only value" />
        </Field>
        <Field
          label="Message"
          description="Shown in the version history."
          className="sm:col-span-2"
        >
          <Textarea placeholder="Add billing tables" />
        </Field>
      </Card>
    </Section>
  );
}

function Badges() {
  return (
    <Section id="badges" title="Badge, Kbd, Tooltip">
      <Card className="grid gap-6 p-4">
        <Specimen label="Badge">
          <Badge>Draft</Badge>
          <Badge variant="primary">Admin</Badge>
          <Badge variant="success">Published</Badge>
          <Badge variant="warning">Out of date</Badge>
          <Badge variant="destructive">Compile error</Badge>
          <Badge variant="info">Pushed by CI</Badge>
          <Badge variant="outline">v12</Badge>
        </Specimen>
        <Specimen label="Kbd">
          <KbdGroup keys={[modKey, "K"]} />
          <KbdGroup keys={[modKey, "Enter"]} />
          <Kbd>Esc</Kbd>
          <Kbd>?</Kbd>
        </Specimen>
        <Specimen label="Tooltip (hover or focus)">
          <Tooltip content="Open the SQL pane" shortcut={[modKey, "1"]}>
            <Button size="icon" aria-label="SQL pane">
              <CodeIcon />
            </Button>
          </Tooltip>
          <Tooltip content="Rename table" side="top">
            <Button size="icon" aria-label="Rename table">
              <EditPencilIcon />
            </Button>
          </Tooltip>
        </Specimen>
      </Card>
    </Section>
  );
}

const crowd = [
  { id: "u-alice", name: "Alice Martin" },
  { id: "u-bob", name: "Bob Durand" },
  { id: "u-chloe", name: "Chloé Bernard" },
  { id: "u-dmitri", name: "Dmitri Volkov" },
  { id: "u-emma", name: "Emma Leroy" },
  { id: "u-farid", name: "Farid Haddad" },
  { id: "u-gwen", name: "Gwen Stacy" },
];

function Avatars() {
  return (
    <Section id="avatars" title="Avatar">
      <Card className="grid gap-6 p-4">
        <Specimen label="Sizes (color is stable per user id)">
          <Avatar id="u-alice" name="Alice Martin" size="sm" />
          <Avatar id="u-alice" name="Alice Martin" size="md" />
          <Avatar id="u-alice" name="Alice Martin" size="lg" />
          <Avatar id="u-alice" name="Alice Martin" size="xl" />
        </Specimen>
        <Specimen label="AvatarStack: who is in this draft">
          <AvatarStack people={crowd.slice(0, 3)} />
          <AvatarStack people={crowd} max={4} size="lg" />
        </Specimen>
      </Card>
    </Section>
  );
}

function TabsDemo() {
  return (
    <Section id="tabs" title="Tabs">
      <Card className="p-4">
        <Tabs defaultValue="sql">
          <TabsList aria-label="Output format">
            <TabsTrigger value="sql">SQL</TabsTrigger>
            <TabsTrigger value="json">JSON</TabsTrigger>
            <TabsTrigger value="dbml">DBML</TabsTrigger>
            <TabsTrigger value="mermaid">Mermaid</TabsTrigger>
          </TabsList>
          <TabsContent value="sql">
            <pre className="overflow-x-auto rounded-md bg-muted p-3 font-mono text-xs">
              CREATE TABLE users (id uuid PRIMARY KEY, email text NOT NULL);
            </pre>
          </TabsContent>
          <TabsContent value="json">
            <pre className="overflow-x-auto rounded-md bg-muted p-3 font-mono text-xs">
              {'{ "tables": [{ "name": "public.users" }] }'}
            </pre>
          </TabsContent>
          <TabsContent value="dbml">
            <pre className="overflow-x-auto rounded-md bg-muted p-3 font-mono text-xs">
              Table users {"{ id uuid [pk] }"}
            </pre>
          </TabsContent>
          <TabsContent value="mermaid">
            <pre className="overflow-x-auto rounded-md bg-muted p-3 font-mono text-xs">
              erDiagram users
            </pre>
          </TabsContent>
        </Tabs>
      </Card>
    </Section>
  );
}

function Overlays() {
  return (
    <Section id="overlays" title="Menu, Popover, Dialog, Toast">
      <Card className="grid gap-6 p-4">
        <Specimen label="Popover (a legend, a small detail)">
          <Popover>
            <PopoverTrigger asChild>
              <Button>Legend</Button>
            </PopoverTrigger>
            <PopoverContent className="grid gap-1.5 text-sm">
              <p className="font-medium text-foreground">Reading the graph</p>
              <p className="text-muted-foreground">
                A key marks the primary key; a link, a foreign key.
              </p>
            </PopoverContent>
          </Popover>
        </Specimen>
        <Specimen label="Menu">
          <Menu>
            <MenuTrigger asChild>
              <Button>
                Actions
                <MoreHorizIcon />
              </Button>
            </MenuTrigger>
            <MenuContent align="start">
              <MenuLabel>Database</MenuLabel>
              <MenuItem>
                <GitBranchIcon />
                New draft
                <MenuShortcut>{modKey} N</MenuShortcut>
              </MenuItem>
              <MenuItem>
                <EditPencilIcon />
                Rename
              </MenuItem>
              <MenuItem disabled>
                <DatabaseIcon />
                Push from CI
              </MenuItem>
              <MenuSeparator />
              <MenuItem variant="destructive">
                <TrashIcon />
                Delete database
              </MenuItem>
            </MenuContent>
          </Menu>
        </Specimen>
        <Specimen label="Dialog">
          <Dialog>
            <DialogTrigger asChild>
              <Button variant="primary">Publish v13</Button>
            </DialogTrigger>
            <DialogContent>
              <DialogHeader>
                <DialogTitle>Publish v13</DialogTitle>
                <DialogDescription>
                  Creates version 13 of billing from this draft and closes it.
                </DialogDescription>
              </DialogHeader>
              <Field label="Message" description="Optional. Shown in the version history.">
                <Input placeholder="Add invoice_lines" />
              </Field>
              <DialogFooter>
                <DialogClose asChild>
                  <Button>Cancel</Button>
                </DialogClose>
                <DialogClose asChild>
                  <Button variant="primary" onClick={() => toast.success("Published v13")}>
                    Publish
                  </Button>
                </DialogClose>
              </DialogFooter>
            </DialogContent>
          </Dialog>
        </Specimen>
        <Specimen label="Toast">
          <Button onClick={() => toast.success("Draft published as v13")}>Success</Button>
          <Button
            onClick={() =>
              toast.error("Could not publish", {
                description: "v13 is out. Update the draft, then publish.",
              })
            }
          >
            Error
          </Button>
          <Button
            onClick={() =>
              toast("Table dropped", {
                action: { label: "Undo", onClick: () => toast("Restored users") },
                duration: 6000,
              })
            }
          >
            With undo
          </Button>
          <Button onClick={() => toast.info("Bob joined the draft")}>Info</Button>
        </Specimen>
      </Card>
    </Section>
  );
}

function Feedback() {
  return (
    <Section id="feedback" title="Alert, Spinner, Skeleton">
      <div className="grid gap-3">
        <Alert variant="info" title="This draft is behind v13">
          Update it to bring in the latest version before publishing.
        </Alert>
        <Alert
          variant="warning"
          title="2 unresolved conflicts"
          action={<Button size="sm">Review</Button>}
        >
          Publishing is paused until each one is resolved.
        </Alert>
        <Alert variant="destructive" title="Compile failed at line 12">
          relation &quot;organisations&quot; does not exist
        </Alert>
        <Alert variant="success" title="Schema compiled" />
      </div>
      <Card className="grid gap-6 p-4">
        <Specimen label="Spinner">
          <Spinner />
          <Spinner className="text-lg text-primary" />
        </Specimen>
        <Specimen label="Skeleton: the exact shape of what loads">
          <div className="flex w-full max-w-md items-center gap-3">
            <Skeleton className="size-8 rounded-full" />
            <div className="grid flex-1 gap-1.5">
              <Skeleton className="h-4 w-32" />
              <Skeleton className="h-3.5 w-full" />
            </div>
          </div>
        </Specimen>
      </Card>
    </Section>
  );
}

function Empty() {
  return (
    <Section id="empty" title="Empty state">
      <Card>
        <EmptyState
          icon={<GitBranchIcon />}
          title="No open drafts"
          description="Start a draft from the latest version to change the schema."
          action={
            <Button variant="primary">
              <PlusIcon />
              New draft
            </Button>
          }
        />
      </Card>
    </Section>
  );
}

function TableAndFriends() {
  const [confirming, setConfirming] = React.useState(false);
  const people = [
    { id: "u-alice", name: "Alice Martin", email: "alice@acme.dev", role: "Admin" },
    { id: "u-bob", name: "Bob Durand", email: "bob@acme.dev", role: "Developer" },
  ];
  return (
    <Section id="table" title="Table, CopyField, ConfirmDialog">
      <Card>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>Member</TableHead>
              <TableHead>Role</TableHead>
              <TableHead className="hidden sm:table-cell">Joined</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {people.map((p) => (
              <TableRow key={p.id}>
                <TableCell className="w-full max-w-0">
                  <div className="flex min-w-0 items-center gap-3">
                    <Avatar id={p.id} name={p.name} size="lg" />
                    <div className="grid min-w-0">
                      <span className="truncate font-medium text-foreground">{p.name}</span>
                      <span className="truncate text-muted-foreground">{p.email}</span>
                    </div>
                  </div>
                </TableCell>
                <TableCell>
                  <Badge>{p.role}</Badge>
                </TableCell>
                <TableCell className="hidden text-muted-foreground tabular sm:table-cell">
                  Oct 15
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </Card>
      <Card className="grid gap-6 p-4">
        <Specimen label="CopyField">
          <CopyField
            value="https://romulus.app/app/invite/0rW9xY3QmZ"
            label="Invitation link"
            className="w-full max-w-md"
          />
        </Specimen>
        <Specimen label="ConfirmDialog">
          <Button onClick={() => setConfirming(true)}>
            <TrashIcon />
            Remove member…
          </Button>
          <ConfirmDialog
            open={confirming}
            onOpenChange={setConfirming}
            title="Remove Bob Durand from Acme?"
            description="They lose access to its databases right away. You can invite them again later."
            confirmLabel="Remove member"
            destructive
            onConfirm={() => new Promise((resolve) => setTimeout(resolve, 600))}
          />
        </Specimen>
      </Card>
    </Section>
  );
}

function ShellLinks() {
  return (
    <Section id="shell" title="App shell">
      <Card className="flex flex-wrap items-center gap-3 p-4">
        <LogoMark className="size-8" />
        <p className="mr-auto text-base text-muted-foreground">
          The signed-in frame with sample data, in each state.
        </p>
        <Button asChild>
          <Link to="/dev/shell">Databases</Link>
        </Button>
        <Button asChild>
          <Link to="/dev/shell" search={{ state: "loading" }}>
            Loading
          </Link>
        </Button>
        <Button asChild>
          <Link to="/dev/shell" search={{ state: "empty" }}>
            Empty
          </Link>
        </Button>
        <Button asChild>
          <Link to="/signin">Sign-in page</Link>
        </Button>
      </Card>
    </Section>
  );
}
