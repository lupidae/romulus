import { ApiError } from "@/api/client";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Field } from "@/components/ui/field";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { toast } from "@/components/ui/toaster";

import { useUpdateOrg } from "../mutations";
import type { Org } from "../queries";

/** What the API allows: 0 (none) to 10. */
const CHOICES = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

function choiceLabel(n: number): string {
  return n === 0 ? "None" : n === 1 ? "1 approval" : `${n} approvals`;
}

/**
 * "Approvals required to publish": how many members who did not edit a
 * draft must approve it first. Saved on change (admins); others read it.
 */
export function ApprovalsCard({ org, editable }: { org: Org; editable: boolean }) {
  const update = useUpdateOrg(org.slug);
  const required = org.approvals_required;
  return (
    <Card className="max-w-2xl">
      <CardHeader>
        <div className="grid gap-0.5">
          <CardTitle>Approvals</CardTitle>
          <p className="text-sm text-muted-foreground">
            Publishing a draft waits for approvals from members who did not edit it; any edit resets
            them. Pushes from CI and imports are never held. Admins can publish without them, saying
            why.
          </p>
        </div>
      </CardHeader>
      <CardContent>
        <Field
          label="Approvals required to publish"
          description={
            editable
              ? required === 0
                ? "Drafts publish without anyone approving them."
                : "Reviewers and developers approve from the draft's editor."
              : "Only admins can change it."
          }
        >
          <Select
            value={String(required)}
            disabled={!editable || update.isPending}
            onValueChange={(value) => {
              const next = Number(value);
              if (next === required) return;
              update.mutate(
                { approvals_required: next },
                {
                  onSuccess: () =>
                    toast.success(
                      next === 0
                        ? "Drafts no longer need approvals"
                        : `Drafts now need ${choiceLabel(next)} to publish`,
                    ),
                  onError: (error) =>
                    toast.error("Could not save the setting", {
                      description:
                        error instanceof ApiError ? error.message : "Check your connection.",
                    }),
                },
              );
            }}
          >
            <SelectTrigger className="w-48">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {CHOICES.map((n) => (
                <SelectItem key={n} value={String(n)}>
                  {choiceLabel(n)}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>
      </CardContent>
    </Card>
  );
}
