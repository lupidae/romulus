import * as React from "react";

import { ApiError } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Label } from "@/components/ui/label";
import { Switch } from "@/components/ui/switch";
import { toast } from "@/components/ui/toaster";

import { useUpdateOrg } from "../mutations";
import type { Org } from "../queries";

/**
 * "Publish only after a passing test": a draft's migration must pass Test
 * on data (up and down on generated rows) before it is published. Saved on
 * change (admins); others read it.
 */
export function TestsCard({ org, editable }: { org: Org; editable: boolean }) {
  const update = useUpdateOrg(org.slug);
  const id = React.useId();
  const on = org.require_passing_test;
  return (
    <Card className="max-w-2xl">
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle>Migration tests</CardTitle>
          <p className="text-sm text-muted-foreground">
            Test on data runs a draft&apos;s migration up and down on generated rows. Pushes and
            imports are never held.
          </p>
        </div>
      </CardHeader>
      <CardContent>
        <div className="flex items-start gap-3">
          <Switch
            id={id}
            checked={on}
            disabled={!editable || update.isPending}
            aria-describedby={`${id}-hint`}
            className="mt-0.5"
            onCheckedChange={(next) =>
              update.mutate(
                { require_passing_test: next },
                {
                  onSuccess: () =>
                    toast.success(
                      next
                        ? "Drafts now publish only after a passing test"
                        : "Drafts no longer need a passing test",
                    ),
                  onError: (error) =>
                    toast.error("Could not save the setting", {
                      description:
                        error instanceof ApiError ? error.message : "Check your connection.",
                    }),
                },
              )
            }
          />
          <div className="grid gap-0.5">
            <Label htmlFor={id}>Publish only after a passing test</Label>
            <p id={`${id}-hint`} className="text-sm text-muted-foreground">
              {editable
                ? "Drafts publish once a test of their current text passes. Postgres 17 databases only."
                : "Only admins can change it."}
            </p>
          </div>
        </div>
      </CardContent>
    </Card>
  );
}
