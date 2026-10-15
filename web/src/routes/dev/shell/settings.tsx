import { createFileRoute } from "@tanstack/react-router";

import { PageHeader } from "@/components/shell/page-header";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { toast } from "@/components/ui/toaster";

export const Route = createFileRoute("/dev/shell/settings")({
  head: () => ({ meta: [{ title: "Settings – romulus" }] }),
  component: Settings,
});

function Settings() {
  return (
    <>
      <PageHeader title="Settings" description="The organization's name and address." />
      <Card className="max-w-xl">
        <form
          className="grid gap-5 p-4"
          onSubmit={(e) => {
            e.preventDefault();
            toast.success("Settings saved");
          }}
        >
          <Field label="Name">
            <Input defaultValue="Acme" />
          </Field>
          <Field label="URL" description="Changing it breaks existing links to this organization.">
            <Input defaultValue="acme" className="font-mono" />
          </Field>
          <div className="flex justify-end">
            <Button variant="primary" type="submit">
              Save changes
            </Button>
          </div>
        </form>
      </Card>
    </>
  );
}
