import { Link } from "@tanstack/react-router";
import * as React from "react";

import { LogoMark } from "@/components/brand/logo";
import { SchemaPreview } from "@/components/shell/schema-preview";
import { Skeleton } from "@/components/ui/skeleton";
import { LegalLinks } from "@/features/legal/components/legal-links";
import { cn } from "@/lib/utils";

interface AuthPageProps {
  title: React.ReactNode;
  description?: React.ReactNode;
  children?: React.ReactNode;
  /** Under the form: the way to the other auth pages. */
  footer?: React.ReactNode;
  /** Move focus to the title on mount: a new step (sent, done) is a new screen. */
  focusTitle?: boolean;
  /** The Terms and the Privacy Policy at the bottom (the hosted service); off where the page links them itself. */
  legal?: boolean;
}

/**
 * The frame of every sign-in page (sign in, sign up, forgotten password,
 * one-time links): the form on the left, a schema on the right on wide
 * screens. One column, centred, at phone width.
 */
export function AuthPage({
  title,
  description,
  children,
  footer,
  focusTitle,
  legal = true,
}: AuthPageProps) {
  const heading = React.useRef<HTMLHeadingElement>(null);
  React.useEffect(() => {
    if (focusTitle) heading.current?.focus();
  }, [focusTitle]);
  return (
    <div className="grid min-h-dvh bg-background lg:grid-cols-[minmax(0,1fr)_minmax(0,1.1fr)]">
      <main className="flex flex-col px-6 py-6 sm:px-10">
        <div className="flex flex-1 items-center justify-center pb-10">
          <div className="w-full max-w-sm">
            <Link
              to="/"
              aria-label="romulus home"
              className="inline-flex rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring"
            >
              <LogoMark className="size-10" />
            </Link>
            <h1
              ref={heading}
              tabIndex={focusTitle ? -1 : undefined}
              className="mt-8 text-2xl font-semibold text-balance text-foreground outline-none"
            >
              {title}
            </h1>
            {description && (
              <p className="mt-2 text-base text-pretty text-muted-foreground">{description}</p>
            )}
            {children && <div className="mt-8">{children}</div>}
            {footer && <div className="mt-6 text-sm text-muted-foreground">{footer}</div>}
          </div>
        </div>
        {legal && <LegalLinks className="justify-center" />}
      </main>

      <aside
        aria-hidden
        className="relative hidden overflow-hidden border-l border-border bg-muted lg:block"
      >
        <SchemaPreview />
      </aside>
    </div>
  );
}

/** A form on an auth page: fields, then its one primary action, full width. */
export function AuthForm({ className, ...props }: React.ComponentProps<"form">) {
  return <form noValidate className={cn("grid gap-4", className)} {...props} />;
}

/** While a one-time link is checked: the shape of the form it opens. */
export function AuthPageSkeleton() {
  return (
    <div className="grid min-h-dvh bg-background lg:grid-cols-[minmax(0,1fr)_minmax(0,1.1fr)]">
      <main className="flex items-center justify-center px-6 py-6 sm:px-10" aria-busy>
        <div className="grid w-full max-w-sm gap-4">
          <Skeleton className="size-10 rounded-lg" />
          <Skeleton className="mt-4 h-7 w-56" />
          <Skeleton className="h-4 w-full" />
          <Skeleton className="mt-4 h-9 w-full" />
          <Skeleton className="h-9 w-full" />
          <Skeleton className="h-10 w-full" />
        </div>
      </main>
      <aside aria-hidden className="hidden border-l border-border bg-muted lg:block" />
    </div>
  );
}

/** A link inside auth-page copy (to another auth page, or out). */
export const authLinkClass =
  "font-medium text-foreground underline underline-offset-4 decoration-border-strong hover:decoration-foreground rounded-sm outline-none focus-visible:ring-2 focus-visible:ring-ring";
