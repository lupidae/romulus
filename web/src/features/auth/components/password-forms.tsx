import { Link, useNavigate } from "@tanstack/react-router";

import { ApiError } from "@/api/client";
import * as React from "react";

import { Alert } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Field } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { PasswordInput } from "@/components/ui/password-input";

import { formatWait, MIN_PASSWORD_LENGTH, retryAfter, usePasswordSignIn } from "../password";
import { afterSignIn } from "../session";
import { AuthForm, authLinkClass } from "./auth-page";

/** The hint under every new-password field. */
export const NEW_PASSWORD_HINT = `At least ${MIN_PASSWORD_LENGTH} characters. A few unrelated words make a strong password.`;

/** A new password: the rule as its hint, the API's (or the form's) objection instead. */
export function NewPasswordField({
  label = "Password",
  value,
  onChange,
  error,
  inputRef,
}: {
  label?: string;
  value: string;
  onChange: (value: string) => void;
  error?: string;
  inputRef?: React.Ref<HTMLInputElement>;
}) {
  return (
    <Field label={label} description={NEW_PASSWORD_HINT} error={error}>
      <PasswordInput
        ref={inputRef}
        name="new-password"
        autoComplete="new-password"
        required
        minLength={MIN_PASSWORD_LENGTH}
        value={value}
        onChange={(event) => onChange(event.target.value)}
      />
    </Field>
  );
}

/**
 * For password managers: the account a new password belongs to, in a field
 * they read (`autocomplete="username"`) but people neither see nor reach.
 */
export function UsernameHint({ email }: { email: string }) {
  return (
    <Input
      type="email"
      name="username"
      autoComplete="username"
      value={email}
      readOnly
      tabIndex={-1}
      aria-hidden
      className="sr-only"
    />
  );
}

/**
 * Email and password. The API answers the same for an unknown address and a
 * wrong password, and so does this form; a locked address says how long to
 * wait, and that a reset works at once.
 */
export function PasswordSignInForm({
  next,
  canReset,
  defaultEmail,
}: {
  next?: string;
  canReset: boolean;
  defaultEmail?: string;
}) {
  const navigate = useNavigate();
  const signIn = usePasswordSignIn();
  const [email, setEmail] = React.useState(defaultEmail ?? "");
  const [password, setPassword] = React.useState("");
  const [missing, setMissing] = React.useState<{ email?: string; password?: string }>({});
  const emailRef = React.useRef<HTMLInputElement>(null);
  const passwordRef = React.useRef<HTMLInputElement>(null);
  // The first field to fill: the address, or the password once it is known.
  React.useEffect(() => (defaultEmail ? passwordRef : emailRef).current?.focus(), [defaultEmail]);

  const failure = signIn.error;
  const wait = retryAfter(failure);
  const reset = canReset ? (
    <>
      , or{" "}
      <Link
        to="/signin/forgot"
        search={{ email: email.trim() || undefined }}
        className={authLinkClass}
      >
        reset your password
      </Link>
    </>
  ) : null;

  return (
    <AuthForm
      aria-label="Sign in with your email"
      onSubmit={(event) => {
        event.preventDefault();
        const problems = {
          email: email.trim() ? undefined : "Enter your email address.",
          password: password ? undefined : "Enter your password.",
        };
        setMissing(problems);
        if (problems.email) return emailRef.current?.focus();
        if (problems.password) return passwordRef.current?.focus();
        signIn.mutate(
          { email: email.trim(), password },
          {
            onSuccess: () => void navigate({ href: afterSignIn(next), replace: true }),
            onError: () => {
              setPassword("");
              // Announced (role=alert), then back to the field to retry.
              requestAnimationFrame(() => passwordRef.current?.focus());
            },
          },
        );
      }}
    >
      {failure && (
        <div>
          {wait !== undefined ? (
            <Alert variant="warning" title="Too many attempts">
              Try again in {formatWait(wait)}
              {reset}.
            </Alert>
          ) : failure instanceof ApiError && failure.code === "invalid_credentials" ? (
            <Alert variant="destructive" title="Email or password is incorrect">
              Try again{reset}.
            </Alert>
          ) : (
            <Alert variant="destructive" title="Could not sign in">
              {failure instanceof ApiError
                ? failure.message
                : "romulus did not answer. Check your connection and try again."}
            </Alert>
          )}
        </div>
      )}
      <Field label="Email" error={missing.email}>
        <Input
          ref={emailRef}
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
      <div className="relative">
        <Field label="Password" error={missing.password}>
          <PasswordInput
            ref={passwordRef}
            name="password"
            autoComplete="current-password"
            required
            value={password}
            onChange={(event) => setPassword(event.target.value)}
          />
        </Field>
        {canReset && (
          <Link
            to="/signin/forgot"
            search={{ email: email.trim() || undefined }}
            className="absolute top-0 right-0 rounded-sm text-sm text-muted-foreground outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring"
          >
            Forgot password?
          </Link>
        )}
      </div>
      <Button
        type="submit"
        variant="primary"
        size="lg"
        className="mt-2 w-full"
        loading={signIn.isPending}
      >
        Sign in
      </Button>
    </AuthForm>
  );
}
