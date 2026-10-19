import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import * as React from "react";

import { ApiError } from "@/api/client";
import { MailIcon } from "@/components/icons";
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
import { NewPasswordField } from "@/features/auth/components/password-forms";
import {
  formErrors,
  formatWait,
  newPasswordProblem,
  retryAfter,
  useSignUp,
} from "@/features/auth/password";
import { authMethodsQuery, instanceQuery } from "@/features/auth/queries";
import { afterSignIn, appPath } from "@/features/auth/session";
import { invitationPreviewQuery } from "@/features/orgs/queries";
import { safePath } from "@/lib/safe-path";

interface NewAccountSearch {
  /** The invitation link's token this sign-up comes from. */
  invitation?: string;
  next?: string;
}

// Creating a password account: from an invitation (the address is the
// invited one), or by anyone when sign-up is open. With mail, a link
// finishes it (receiving it proves the address); without, the invitation
// vouches and the account is made at once.
export const Route = createFileRoute("/signin_/new")({
  validateSearch: (search: Record<string, unknown>): NewAccountSearch => ({
    invitation:
      typeof search.invitation === "string" && /^[\w-]{43}$/.test(search.invitation)
        ? search.invitation
        : undefined,
    next: safePath(search.next),
  }),
  head: () => ({ meta: [{ title: "Create your account – romulus" }] }),
  component: NewAccount,
});

function NewAccount() {
  const { invitation: token, next } = Route.useSearch();
  const methods = useQuery(authMethodsQuery);
  const instance = useQuery(instanceQuery);
  const invitation = useQuery({ ...invitationPreviewQuery(token ?? ""), enabled: !!token });

  if (methods.isPending || instance.isPending || (token && invitation.isPending))
    return <AuthPageSkeleton />;
  const signInHere = (
    <>
      Already have an account?{" "}
      <Link
        to="/signin"
        search={{ next: token ? appPath(`/invite/${token}`) : next }}
        className={authLinkClass}
      >
        Sign in
      </Link>
    </>
  );
  if (!methods.data?.password) {
    return (
      <AuthPage
        title="Accounts come from your identity provider"
        description="This server signs people in with single sign-on: there is no romulus password to create."
        footer={signInHere}
      />
    );
  }
  const invited = token && invitation.data?.email ? invitation.data : undefined;
  if (token && !invited) {
    return (
      <AuthPage
        title="This invitation does not work any more"
        description="It was used, revoked, replaced or it expired. Ask an admin of the organization for a new one."
        footer={signInHere}
      />
    );
  }
  if (!invited && methods.data.sign_up !== "open") {
    return (
      <AuthPage
        title="Accounts start from an invitation"
        description="Ask an admin of your organization to invite you: the invitation's link creates your account."
        footer={signInHere}
      />
    );
  }
  return (
    <SignUpForm
      token={token}
      invitedEmail={invited?.email ?? undefined}
      org={invited?.org.name}
      mail={instance.data?.mail === true}
      next={next}
      footer={signInHere}
    />
  );
}

function SignUpForm({
  token,
  invitedEmail,
  org,
  mail,
  next,
  footer,
}: {
  token?: string;
  invitedEmail?: string;
  org?: string;
  mail: boolean;
  next?: string;
  footer: React.ReactNode;
}) {
  const navigate = useNavigate();
  const signUp = useSignUp();
  const [name, setName] = React.useState("");
  const [email, setEmail] = React.useState(invitedEmail ?? "");
  const [password, setPassword] = React.useState("");
  const [problems, setProblems] = React.useState<Record<string, string | undefined>>({});
  const [sentTo, setSentTo] = React.useState<string>();
  const nameRef = React.useRef<HTMLInputElement>(null);
  const emailRef = React.useRef<HTMLInputElement>(null);
  const passwordRef = React.useRef<HTMLInputElement>(null);
  React.useEffect(() => nameRef.current?.focus(), []);

  if (sentTo) {
    return (
      <AuthPage
        focusTitle
        title="Check your email"
        description={
          <>
            We sent a link to <span className="font-medium text-foreground">{sentTo}</span> to
            choose your password. It works once, for 24 hours.
          </>
        }
        footer={footer}
      >
        <div className="flex items-start gap-3 rounded-lg border border-border bg-card p-4 text-sm text-muted-foreground">
          <MailIcon className="mt-0.5 size-4 shrink-0 text-foreground" />
          <p>
            Nothing yet? Check your spam folder. If you already have an account, the email says so.
          </p>
        </div>
      </AuthPage>
    );
  }

  const failure = signUp.error ? formErrors(signUp.error) : undefined;
  const wait = retryAfter(signUp.error);
  const exists = signUp.error instanceof ApiError && signUp.error.code === "account_exists";
  const fieldError = (field: string) => problems[field] ?? failure?.fields[field];

  return (
    <AuthPage
      title={org ? `Join ${org} on romulus` : "Create your romulus account"}
      description={
        mail
          ? "We will email you a link to choose your password."
          : "Use the address you were invited with."
      }
      footer={footer}
    >
      <AuthForm
        onSubmit={(event) => {
          event.preventDefault();
          const local = {
            name: name.trim() ? undefined : "Enter your name, as your team knows you.",
            email: email.trim() ? undefined : "Enter your email address.",
            password: mail ? undefined : newPasswordProblem(password),
          };
          setProblems(local);
          if (local.name) return nameRef.current?.focus();
          if (local.email) return emailRef.current?.focus();
          if (local.password) return passwordRef.current?.focus();
          signUp.mutate(
            {
              name: name.trim(),
              email: email.trim(),
              password: mail ? undefined : password,
              invitation: token,
            },
            {
              onSuccess: (result) => {
                if (result.outcome === "email_sent") return setSentTo(email.trim());
                if (result.org) {
                  toast.success(`Welcome to ${result.org.name}`, {
                    description: `You joined as ${result.org.role === "admin" ? "an" : "a"} ${result.org.role}.`,
                  });
                  void navigate({ to: "/$org", params: { org: result.org.slug }, replace: true });
                } else {
                  void navigate({ href: afterSignIn(next), replace: true });
                }
              },
            },
          );
        }}
      >
        {exists ? (
          <Alert variant="info" title="You already have an account">
            Sign in with {email.trim()} instead, then open the invitation again.
          </Alert>
        ) : (
          failure?.message && (
            <Alert
              variant={wait === undefined ? "destructive" : "warning"}
              title="Could not create the account"
            >
              {wait === undefined
                ? failure.message
                : `Too many attempts. Try again in ${formatWait(wait)}.`}
            </Alert>
          )
        )}
        {failure?.fields.invitation && (
          <Alert variant="destructive" title="An invitation is needed">
            {failure.fields.invitation}
          </Alert>
        )}
        <Field label="Name" error={fieldError("name")}>
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
        <Field label="Email" error={fieldError("email")}>
          <Input
            ref={emailRef}
            type="email"
            name="email"
            autoComplete={mail ? "email" : "username"}
            inputMode="email"
            spellCheck={false}
            autoCapitalize="off"
            required
            readOnly={!!invitedEmail}
            // Fixed by the invitation: it reads as set, not as a field to fill.
            className={invitedEmail ? "bg-muted text-muted-foreground" : undefined}
            value={email}
            onChange={(event) => setEmail(event.target.value)}
          />
        </Field>
        {!mail && (
          <NewPasswordField
            inputRef={passwordRef}
            value={password}
            onChange={setPassword}
            error={fieldError("password")}
          />
        )}
        <Button
          type="submit"
          variant="primary"
          size="lg"
          className="mt-2 w-full"
          loading={signUp.isPending}
        >
          {mail ? "Email me a link" : org ? `Create account and join ${org}` : "Create account"}
        </Button>
      </AuthForm>
    </AuthPage>
  );
}
