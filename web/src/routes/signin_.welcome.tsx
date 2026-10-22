import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
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
  signUpLinkQuery,
  useFinishSignUp,
} from "@/features/auth/password";
import { afterSignIn, takeSignUpNext } from "@/features/auth/session";

// Where an emailed sign-up link lands (`/app/signin/welcome#token=…`):
// receiving it proved the address; choose a password and the account exists.
export const Route = createFileRoute("/signin_/welcome")({
  head: () => ({ meta: [{ title: "Finish creating your account – romulus" }] }),
  component: Welcome,
});

function Welcome() {
  const [token] = React.useState(readLinkToken);
  React.useEffect(forgetLinkToken, []);
  const link = useQuery({ ...signUpLinkQuery(token ?? ""), enabled: !!token });

  if (token && link.isPending) return <AuthPageSkeleton />;
  if (!token || !link.isSuccess) {
    const offline = link.isError && !(link.error instanceof ApiError);
    return (
      <AuthPage
        title={offline ? "Could not open this link" : "This link does not work any more"}
        description={
          offline
            ? "romulus is not answering. Check your connection, then open the link again."
            : "Sign-up links work once, for 24 hours. If you already used it, your account exists: sign in."
        }
        footer={
          <Link to="/signin" className={authLinkClass}>
            Go to sign in
          </Link>
        }
      />
    );
  }
  return <Finish token={token} email={link.data.email} name={link.data.name} org={link.data.org} />;
}

function Finish({
  token,
  email,
  name: given,
  org,
}: {
  token: string;
  email: string;
  name: string;
  org?: string | null;
}) {
  const navigate = useNavigate();
  const finish = useFinishSignUp();
  const [name, setName] = React.useState(given);
  const [password, setPassword] = React.useState("");
  const [problems, setProblems] = React.useState<{ name?: string; password?: string }>({});
  const nameRef = React.useRef<HTMLInputElement>(null);
  const passwordRef = React.useRef<HTMLInputElement>(null);
  // The name came with the link: the password is what is left to give.
  React.useEffect(() => passwordRef.current?.focus(), []);
  const failure = finish.error ? formErrors(finish.error) : undefined;
  const exists = finish.error instanceof ApiError && finish.error.code === "account_exists";

  return (
    <AuthPage
      title={org ? `Finish joining ${org}` : "Finish creating your account"}
      description={
        <>
          Signing up as <span className="font-medium text-foreground">{email}</span>.
        </>
      }
    >
      <AuthForm
        onSubmit={(event) => {
          event.preventDefault();
          const local = {
            name: name.trim() ? undefined : "Enter your name, as your team knows you.",
            password: newPasswordProblem(password),
          };
          setProblems(local);
          if (local.name) return nameRef.current?.focus();
          if (local.password) return passwordRef.current?.focus();
          finish.mutate(
            { token, name: name.trim(), password },
            {
              onSuccess: (result) => {
                if (result.org) {
                  toast.success(`Welcome to ${result.org.name}`, {
                    description: `You joined as ${result.org.role === "admin" ? "an" : "a"} ${result.org.role}.`,
                  });
                  void navigate({ to: "/$org", params: { org: result.org.slug }, replace: true });
                } else {
                  toast.success("Welcome to romulus");
                  void navigate({ href: afterSignIn(takeSignUpNext()), replace: true });
                }
              },
            },
          );
        }}
      >
        <UsernameHint email={email} />
        {exists ? (
          <Alert variant="info" title="You already have an account">
            {email} has one now.{" "}
            <Link to="/signin" search={{ email }} className={authLinkClass}>
              Sign in
            </Link>{" "}
            instead.
          </Alert>
        ) : (
          failure?.message && (
            <Alert variant="destructive" title="Could not create the account">
              {failure.message}
            </Alert>
          )
        )}
        <Field label="Name" error={problems.name ?? failure?.fields.name}>
          <Input
            ref={nameRef}
            name="name"
            autoComplete="name"
            required
            maxLength={100}
            value={name}
            onChange={(event) => setName(event.target.value)}
          />
        </Field>
        <NewPasswordField
          inputRef={passwordRef}
          value={password}
          onChange={setPassword}
          error={problems.password ?? failure?.fields.password}
        />
        <Button
          type="submit"
          variant="primary"
          size="lg"
          className="mt-2 w-full"
          loading={finish.isPending}
        >
          {org ? `Create account and join ${org}` : "Create account"}
        </Button>
      </AuthForm>
    </AuthPage>
  );
}
