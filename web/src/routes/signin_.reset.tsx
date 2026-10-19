import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { toast } from "@/components/ui/toaster";
import {
  AuthForm,
  AuthPage,
  AuthPageSkeleton,
  authLinkClass,
} from "@/features/auth/components/auth-page";
import { NewPasswordField, UsernameHint } from "@/features/auth/components/password-forms";
import {
  formErrors,
  forgetLinkToken,
  newPasswordProblem,
  readLinkToken,
  resetLinkQuery,
  useResetPassword,
} from "@/features/auth/password";
import { authMethodsQuery } from "@/features/auth/queries";

// Where a reset link lands (`/app/signin/reset#token=…`), and the first
// admin's link from `romulus-api setup`: choose a password, signed in after.
export const Route = createFileRoute("/signin_/reset")({
  head: () => ({ meta: [{ title: "Choose a password – romulus" }] }),
  component: ResetPassword,
});

function ResetPassword() {
  const [token] = React.useState(readLinkToken);
  React.useEffect(forgetLinkToken, []);
  const link = useQuery({ ...resetLinkQuery(token ?? ""), enabled: !!token });

  if (!token) return <DeadLink />;
  if (link.isPending) return <AuthPageSkeleton />;
  if (link.isError) return <DeadLink offline={!(link.error instanceof ApiError)} />;
  return <ChoosePassword token={token} email={link.data.email} first={link.data.first_password} />;
}

function ChoosePassword({ token, email, first }: { token: string; email: string; first: boolean }) {
  const navigate = useNavigate();
  const reset = useResetPassword();
  const [password, setPassword] = React.useState("");
  const [problem, setProblem] = React.useState<string>();
  const input = React.useRef<HTMLInputElement>(null);
  React.useEffect(() => input.current?.focus(), []);
  const failure = reset.error ? formErrors(reset.error) : undefined;
  const gone = reset.error instanceof ApiError && reset.error.status === 404;
  if (gone) return <DeadLink />;

  return (
    <AuthPage
      title={first ? "Set your password" : "Choose a new password"}
      description={
        <>
          For <span className="font-medium text-foreground">{email}</span>.{" "}
          {first
            ? "You will sign in with it from now on."
            : "You will be signed out everywhere else."}
        </>
      }
    >
      <AuthForm
        onSubmit={(event) => {
          event.preventDefault();
          const local = newPasswordProblem(password);
          setProblem(local);
          if (local) return input.current?.focus();
          reset.mutate(
            { token, password },
            {
              onSuccess: () => {
                toast.success(first ? "Password set. Welcome to romulus." : "Password changed", {
                  description: first
                    ? undefined
                    : "You are signed in; other sessions were signed out.",
                });
                void navigate({ to: "/", replace: true });
              },
              onError: () => requestAnimationFrame(() => input.current?.focus()),
            },
          );
        }}
      >
        <UsernameHint email={email} />
        {failure?.message && (
          <Alert variant="destructive" title="Could not save the password">
            {failure.message}
          </Alert>
        )}
        <NewPasswordField
          label={first ? "Password" : "New password"}
          inputRef={input}
          value={password}
          onChange={setPassword}
          error={problem ?? failure?.fields.password}
        />
        <Button
          type="submit"
          variant="primary"
          size="lg"
          className="mt-2 w-full"
          loading={reset.isPending}
        >
          {first ? "Set password and sign in" : "Save and sign in"}
        </Button>
      </AuthForm>
    </AuthPage>
  );
}

/** A link that is used, expired, replaced by a newer one, or mistyped. */
function DeadLink({ offline = false }: { offline?: boolean }) {
  const methods = useQuery(authMethodsQuery);
  if (offline) {
    return (
      <AuthPage title="Could not open this link" footer={<SignInLink />}>
        <Alert variant="warning" title="romulus is not answering">
          Check your connection, then open the link again.
        </Alert>
      </AuthPage>
    );
  }
  return (
    <AuthPage
      title="This link does not work any more"
      description="Password links work once, for a limited time, and a newer one replaces the older. Ask for a new one."
      footer={<SignInLink />}
    >
      {methods.data?.password_reset ? (
        <Button variant="primary" size="lg" className="w-full" asChild>
          <Link to="/signin/forgot">Send me a new link</Link>
        </Button>
      ) : (
        <Alert variant="info" title="Ask whoever runs this server">
          They can make you a new link with{" "}
          <code className="font-mono text-sm text-foreground">romulus-api password-link</code>.
        </Alert>
      )}
    </AuthPage>
  );
}

function SignInLink() {
  return (
    <Link to="/signin" className={authLinkClass}>
      Back to sign in
    </Link>
  );
}
