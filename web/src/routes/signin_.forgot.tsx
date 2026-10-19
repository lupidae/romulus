import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import * as React from "react";

import { MailIcon } from "@/components/icons";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { AuthForm, AuthPage, authLinkClass } from "@/features/auth/components/auth-page";
import { formErrors, formatWait, retryAfter, useForgotPassword } from "@/features/auth/password";
import { authMethodsQuery } from "@/features/auth/queries";

interface ForgotSearch {
  /** Pre-filled from the sign-in form. */
  email?: string;
}

// "Forgot password?": a reset link by email. The answer is the same whether
// the address has an account or not (the API's, and this page's).
export const Route = createFileRoute("/signin_/forgot")({
  validateSearch: (search: Record<string, unknown>): ForgotSearch => ({
    email: typeof search.email === "string" ? search.email.slice(0, 254) : undefined,
  }),
  head: () => ({ meta: [{ title: "Reset your password – romulus" }] }),
  component: ForgotPassword,
});

const backToSignIn = (
  <Link to="/signin" className={authLinkClass}>
    Back to sign in
  </Link>
);

function ForgotPassword() {
  const { email: initial } = Route.useSearch();
  const methods = useQuery(authMethodsQuery);
  const forgot = useForgotPassword();
  const [email, setEmail] = React.useState(initial ?? "");
  const [missing, setMissing] = React.useState<string>();
  const [sentTo, setSentTo] = React.useState<string>();
  const input = React.useRef<HTMLInputElement>(null);
  const formShown = methods.data?.password_reset === true && !sentTo;
  React.useEffect(() => {
    if (formShown) input.current?.focus();
  }, [formShown]);

  if (methods.isPending) {
    return (
      <AuthPage title="Reset your password">
        <Skeleton className="h-24 w-full" />
      </AuthPage>
    );
  }
  if (methods.data?.password_reset !== true) {
    return (
      <AuthPage
        title="Reset your password"
        description="This romulus server does not send email, so it cannot mail you a link."
        footer={backToSignIn}
      >
        <Alert variant="info" title="Ask whoever runs this server">
          They can make you a one-time link to choose a new password, with{" "}
          <code className="font-mono text-sm text-foreground">romulus-api password-link</code> and
          your email address.
        </Alert>
      </AuthPage>
    );
  }

  if (sentTo) {
    return (
      <AuthPage
        focusTitle
        title="Check your email"
        description={
          <>
            If an account uses <span className="font-medium text-foreground">{sentTo}</span>, a link
            is on its way. It works once, for 30 minutes.
          </>
        }
        footer={backToSignIn}
      >
        <div className="flex items-start gap-3 rounded-lg border border-border bg-card p-4 text-sm text-muted-foreground">
          <MailIcon className="mt-0.5 size-4 shrink-0 text-foreground" />
          <p>
            Nothing yet? Check your spam folder, or{" "}
            <Button
              variant="link"
              className="h-auto align-baseline text-sm"
              onClick={() => {
                forgot.reset();
                setSentTo(undefined);
              }}
            >
              try again
            </Button>
            .
          </p>
        </div>
      </AuthPage>
    );
  }

  const wait = retryAfter(forgot.error);
  const failure = forgot.error ? formErrors(forgot.error) : undefined;
  return (
    <AuthPage
      title="Reset your password"
      description="We will email you a link to choose a new password."
      footer={backToSignIn}
    >
      <AuthForm
        onSubmit={(event) => {
          event.preventDefault();
          const address = email.trim();
          if (!address) {
            setMissing("Enter your email address.");
            input.current?.focus();
            return;
          }
          setMissing(undefined);
          forgot.mutate(address, { onSuccess: () => setSentTo(address) });
        }}
      >
        {forgot.error && !failure?.fields.email && (
          <Alert
            variant={wait === undefined ? "destructive" : "warning"}
            title="Could not send the link"
          >
            {wait === undefined
              ? failure?.message
              : `Too many requests. Try again in ${formatWait(wait)}.`}
          </Alert>
        )}
        <Field label="Email" error={missing ?? failure?.fields.email}>
          <Input
            ref={input}
            type="email"
            name="email"
            autoComplete="username"
            inputMode="email"
            spellCheck={false}
            autoCapitalize="off"
            required
            value={email}
            onChange={(event) => setEmail(event.target.value)}
          />
        </Field>
        <Button
          type="submit"
          variant="primary"
          size="lg"
          className="mt-2 w-full"
          loading={forgot.isPending}
        >
          Send reset link
        </Button>
      </AuthForm>
    </AuthPage>
  );
}
