import { useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { DiceFiveIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { RadioCard, RadioGroup } from "@/components/ui/radio-group";
import { toast } from "@/components/ui/toaster";
import { Tooltip } from "@/components/ui/tooltip";

import { useCreatePlayground } from "../mutations";
import type { PlaygroundProfile, PlaygroundSize } from "../queries";

/** What the playground is for: a draft (its base version) or a version. */
export type PlaygroundSource =
  | { kind: "draft"; draftId: string; draftName: string; version: number; tables?: number }
  | { kind: "version"; version: number; tables?: number };

interface CreatePlaygroundDialogProps {
  org: string;
  db: string;
  source: PlaygroundSource;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}

const SIZES: { value: PlaygroundSize; label: string; rows: string; time: string }[] = [
  { value: "small", label: "Small", rows: "up to 50,000 rows", time: "about 10\u00a0s" },
  { value: "medium", label: "Medium", rows: "up to 1,000,000 rows", time: "about a minute" },
];

const PROFILES: { value: PlaygroundProfile; label: string; description: string }[] = [
  {
    value: "realistic",
    label: "Realistic",
    description: "Plausible values, skewed like real data.",
  },
  {
    value: "edge_cases",
    label: "Edge cases",
    description: "NULLs, maximum lengths, duplicates: the rows that break migrations.",
  },
];

function randomSeed(): string {
  const bytes = new Uint8Array(3);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * A new playground: which schema it holds, how many rows, which kind, the
 * seed (the same seed gives the same rows). Opens the playground as it
 * builds.
 */
export function CreatePlaygroundDialog({
  org,
  db,
  source,
  open,
  onOpenChange,
}: CreatePlaygroundDialogProps) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>New playground</DialogTitle>
          <DialogDescription>
            A real database holding v{source.version} with generated rows, to query and to try
            {source.kind === "draft" ? " this draft's migration on." : " queries on."}
          </DialogDescription>
        </DialogHeader>
        {open && (
          <CreateForm org={org} db={db} source={source} onCancel={() => onOpenChange(false)} />
        )}
      </DialogContent>
    </Dialog>
  );
}

function CreateForm({
  org,
  db,
  source,
  onCancel,
}: {
  org: string;
  db: string;
  source: PlaygroundSource;
  onCancel: () => void;
}) {
  const navigate = useNavigate();
  const create = useCreatePlayground(org, db);
  const [size, setSize] = React.useState<PlaygroundSize>("small");
  const [profile, setProfile] = React.useState<PlaygroundProfile>("realistic");
  const [seed, setSeed] = React.useState(randomSeed);
  const [error, setError] = React.useState<string | null>(null);
  const seedProblem = /^[0-9a-f]{1,15}$/i.test(seed.trim())
    ? null
    : "Up to 15 hexadecimal digits (0–9, a–f).";

  const submit = (event: React.FormEvent) => {
    event.preventDefault();
    if (seedProblem) return;
    setError(null);
    create.mutate(
      {
        ...(source.kind === "draft" ? { draft_id: source.draftId } : { version: source.version }),
        size,
        profile,
        seed: seed.trim().toLowerCase(),
      },
      {
        onSuccess: (playground) => {
          void navigate({
            to: "/$org/$db/playgrounds/$playground",
            params: { org, db, playground: playground.id },
          });
        },
        onError: (failure) => {
          if (failure instanceof ApiError && failure.code === "playground_exists") {
            toast.info("A playground exists for this already", {
              description: "Opening it.",
            });
            onCancel();
            return;
          }
          setError(
            failure instanceof ApiError ? failure.message : "Something went wrong. Try again.",
          );
        },
      },
    );
  };

  return (
    <form noValidate onSubmit={submit} className="grid gap-5">
      <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 rounded-lg border border-border bg-muted/40 px-3 py-2.5 text-sm">
        <dt className="text-muted-foreground">Schema</dt>
        <dd className="min-w-0 truncate text-foreground">
          v{source.version}
          {source.kind === "draft" && (
            <span className="text-muted-foreground"> · the base of “{source.draftName}”</span>
          )}
        </dd>
        {source.tables !== undefined && (
          <>
            <dt className="text-muted-foreground">Tables</dt>
            <dd className="text-foreground tabular-nums">{source.tables}</dd>
          </>
        )}
      </dl>

      <div className="grid gap-1.5">
        <Label id="playground-rows">Rows</Label>
        <RadioGroup
          aria-labelledby="playground-rows"
          value={size}
          onValueChange={(value) => setSize(value as PlaygroundSize)}
          className="sm:grid-cols-2"
        >
          {SIZES.map((option) => (
            <RadioCard
              key={option.value}
              value={option.value}
              label={option.label}
              description={`${option.rows}, ${option.time}`}
            />
          ))}
        </RadioGroup>
      </div>

      <div className="grid gap-1.5">
        <Label id="playground-values">Values</Label>
        <RadioGroup
          aria-labelledby="playground-values"
          value={profile}
          onValueChange={(value) => setProfile(value as PlaygroundProfile)}
          className="sm:grid-cols-2"
        >
          {PROFILES.map((option) => (
            <RadioCard
              key={option.value}
              value={option.value}
              label={option.label}
              description={option.description}
            />
          ))}
        </RadioGroup>
      </div>

      <div className="flex items-start gap-2">
        <Field
          className="flex-1"
          label="Seed"
          description="The same schema, rows and seed give the same rows."
          error={seedProblem ?? undefined}
        >
          <Input
            value={seed}
            onChange={(e) => setSeed(e.target.value)}
            maxLength={15}
            spellCheck={false}
            autoComplete="off"
            className="font-mono"
          />
        </Field>
        {/* Level with the input, past the label above it. */}
        <Tooltip content="Another seed">
          <Button
            type="button"
            variant="secondary"
            size="icon"
            aria-label="Pick another seed"
            className="mt-[1.625rem]"
            onClick={() => setSeed(randomSeed())}
          >
            <DiceFiveIcon />
          </Button>
        </Tooltip>
      </div>

      {error && (
        <p role="alert" className="text-sm text-destructive">
          {error}
        </p>
      )}

      <DialogFooter>
        <Button type="button" variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" variant="primary" loading={create.isPending}>
          Create playground
        </Button>
      </DialogFooter>
    </form>
  );
}
