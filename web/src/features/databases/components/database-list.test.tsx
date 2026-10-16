import { render, screen, within } from "@testing-library/react";
import type * as React from "react";
import { vi } from "vitest";

import { TooltipProvider } from "@/components/ui/tooltip";

import type { Database } from "../queries";
import { DatabaseList } from "./database-list";

vi.mock("@tanstack/react-router", () => ({
  Link: ({
    children,
    params,
    className,
  }: React.PropsWithChildren<{ params: { db: string }; className?: string }>) => (
    <a href={`/app/acme/${params.db}`} className={className}>
      {children}
    </a>
  ),
}));

const database = (slug: string, latest: Database["latest"]): Database => ({
  id: slug,
  slug,
  name: slug.charAt(0).toUpperCase() + slug.slice(1),
  description: "",
  created_at: "2026-10-15T10:00:00Z",
  updated_at: "2026-10-15T10:00:00Z",
  latest,
  versions: latest ? latest.number : 0,
  next_number: (latest?.number ?? 0) + 1,
  missing: [],
  last_activity_at: "2026-10-15T10:00:00Z",
});

describe("DatabaseList", () => {
  it("says who made each latest version, and when there is none", () => {
    render(
      <TooltipProvider>
        <DatabaseList
          org="acme"
          databases={[
            database("billing", {
              id: "v",
              number: 12,
              origin: "push",
              source_kind: "remus_json",
              message: null,
              created_at: new Date().toISOString(),
              author: { kind: "token", id: "t", name: "GitHub Actions" },
            }),
            database("empty", null),
          ]}
        />
      </TooltipProvider>,
    );
    const [billing, empty] = screen.getAllByRole("link");
    expect(billing).toHaveAttribute("href", "/app/acme/billing");
    expect(within(billing!).getByText("v12")).toBeInTheDocument();
    expect(within(billing!).getByText("GitHub Actions")).toBeInTheDocument();
    expect(within(billing!).getByText("just now")).toBeInTheDocument();
    expect(within(empty!).getByText("No versions yet")).toBeInTheDocument();
  });
});
