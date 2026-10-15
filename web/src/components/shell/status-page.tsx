import { Link, useRouter } from "@tanstack/react-router";
import type * as React from "react";

import { LogoMark } from "@/components/brand/logo";
import { ArrowLeftIcon, RefreshDoubleIcon } from "@/components/icons";
import { Button } from "@/components/ui/button";

interface StatusPageProps {
  code: string;
  title: string;
  description: React.ReactNode;
  actions: React.ReactNode;
}

/** A full-page dead end (404, crash) that always offers a way out. */
function StatusPage({ code, title, description, actions }: StatusPageProps) {
  return (
    <div className="flex min-h-dvh flex-col bg-background">
      <header className="flex h-14 items-center px-5">
        <Link
          to="/"
          aria-label="romulus home"
          className="rounded-md outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <LogoMark />
        </Link>
      </header>
      <main className="flex flex-1 items-center justify-center px-4 pb-24">
        <div className="grid max-w-md justify-items-center gap-5 text-center">
          <p aria-hidden className="text-display font-semibold text-border-strong tabular">
            {code}
          </p>
          <div className="grid gap-2">
            <h1 className="text-2xl font-semibold text-foreground">{title}</h1>
            <p className="text-base text-muted-foreground">{description}</p>
          </div>
          <div className="flex flex-wrap justify-center gap-2">{actions}</div>
        </div>
      </main>
    </div>
  );
}

export function NotFoundPage() {
  const router = useRouter();
  return (
    <StatusPage
      code="404"
      title="Page not found"
      description="The link may be broken, or the page may have moved."
      actions={
        <>
          <Button variant="secondary" onClick={() => router.history.back()}>
            <ArrowLeftIcon />
            Go back
          </Button>
          <Button variant="primary" asChild>
            <Link to="/">Go to romulus</Link>
          </Button>
        </>
      }
    />
  );
}

export function ErrorPage({ error, reset }: { error: unknown; reset: () => void }) {
  const message = error instanceof Error ? error.message : undefined;
  return (
    <StatusPage
      code="500"
      title="Something went wrong"
      description={message ?? "An unexpected error stopped this page from loading."}
      actions={
        <>
          <Button variant="secondary" asChild>
            <Link to="/">Go to romulus</Link>
          </Button>
          <Button variant="primary" onClick={reset}>
            <RefreshDoubleIcon />
            Try again
          </Button>
        </>
      }
    />
  );
}
