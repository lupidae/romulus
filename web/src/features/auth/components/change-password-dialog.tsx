import * as React from "react";

import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Field } from "@/components/ui/field";
import { PasswordInput } from "@/components/ui/password-input";
import { toast } from "@/components/ui/toaster";

import {
  formErrors,
  formatWait,
  newPasswordProblem,
  retryAfter,
  useChangePassword,
} from "../password";
import { NewPasswordField, UsernameHint } from "./password-forms";

/**
 * A password account changes its password: the current one, then the new.
 * Other browsers are signed out; this one stays.
 */
export function ChangePasswordDialog({
  email,
  open,
  onOpenChange,
}: {
  email: string;
  open: boolean;
  onOpenChange: (open: boolean) => void;
}) {
  const change = useChangePassword();
  const [current, setCurrent] = React.useState("");
  const [next, setNext] = React.useState("");
  const [problems, setProblems] = React.useState<{ current?: string; next?: string }>({});
  const currentRef = React.useRef<HTMLInputElement>(null);
  const nextRef = React.useRef<HTMLInputElement>(null);
  const failure = change.error ? formErrors(change.error) : undefined;
  const wait = retryAfter(change.error);

  const close = (value: boolean) => {
    onOpenChange(value);
    if (!value) {
      setCurrent("");
      setNext("");
      setProblems({});
      change.reset();
    }
  };

  return (
    <Dialog open={open} onOpenChange={close}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Change password</DialogTitle>
          <DialogDescription>
            Other browsers signed in as {email} are signed out; this one stays.
          </DialogDescription>
        </DialogHeader>
        <form
          noValidate
          className="grid gap-4"
          onSubmit={(event) => {
            event.preventDefault();
            const local = {
              current: current ? undefined : "Enter your current password.",
              next: newPasswordProblem(next),
            };
            setProblems(local);
            if (local.current) return currentRef.current?.focus();
            if (local.next) return nextRef.current?.focus();
            change.mutate(
              { current_password: current, new_password: next },
              {
                onSuccess: () => {
                  toast.success("Password changed", {
                    description: "Other browsers were signed out.",
                  });
                  close(false);
                },
                onError: (error) => {
                  const fields = formErrors(error).fields;
                  if (fields.current_password) {
                    setCurrent("");
                    requestAnimationFrame(() => currentRef.current?.focus());
                  } else if (fields.new_password) {
                    requestAnimationFrame(() => nextRef.current?.focus());
                  }
                },
              },
            );
          }}
        >
          <UsernameHint email={email} />
          {(wait !== undefined || (failure?.message && !Object.keys(failure.fields).length)) && (
            <Alert
              variant={wait === undefined ? "destructive" : "warning"}
              title="Could not change it"
            >
              {wait === undefined
                ? failure?.message
                : `Too many wrong passwords. Try again in ${formatWait(wait)}.`}
            </Alert>
          )}
          <Field
            label="Current password"
            error={problems.current ?? failure?.fields.current_password}
          >
            <PasswordInput
              ref={currentRef}
              name="current-password"
              autoComplete="current-password"
              required
              value={current}
              onChange={(event) => setCurrent(event.target.value)}
            />
          </Field>
          <NewPasswordField
            label="New password"
            inputRef={nextRef}
            value={next}
            onChange={setNext}
            error={problems.next ?? failure?.fields.new_password}
          />
          <DialogFooter>
            <DialogClose asChild>
              <Button>Cancel</Button>
            </DialogClose>
            <Button type="submit" variant="primary" loading={change.isPending}>
              Change password
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
