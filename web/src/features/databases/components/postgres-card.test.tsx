import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { vi } from "vitest";

import { TooltipProvider } from "@/components/ui/tooltip";

import type { Database } from "../queries";
import { PostgresCard } from "./postgres-card";

const database: Database = {
  id: "d",
  slug: "rooms",
  name: "Rooms",
  description: "",
  pg_major: 17,
  created_at: "2026-10-15T10:00:00Z",
  updated_at: "2026-10-15T10:00:00Z",
  latest: null,
  versions: 3,
  next_number: 4,
  missing: [],
  last_activity_at: "2026-10-15T10:00:00Z",
};

const SQL =
  "CREATE TABLE bookings (\n  room int,\n  EXCLUDE USING gist (room WITH =)\n) PARTITION BY LIST (room);";

/** The API as the card sees it: the dry run per major, the switch, the majors. */
function stubApi() {
  const calls: { method: string; path: string; body: unknown }[] = [];
  const json = (status: number, body: unknown) =>
    new Response(JSON.stringify(body), {
      status,
      headers: { "content-type": "application/json" },
    });
  vi.stubGlobal("fetch", async (request: Request) => {
    const path = new URL(request.url).pathname;
    const body: unknown = request.method === "GET" ? undefined : await request.json();
    calls.push({ method: request.method, path, body });
    if (path === "/api/postgres-majors") return json(200, []);
    const major = (body as { pg_major: number }).pg_major;
    if (path.endsWith("/major/check")) {
      return major === 15
        ? json(200, {
            pg_major: 15,
            against: 3,
            against_major: 17,
            compiles: false,
            sql: SQL,
            changes: null,
            diagnostics: [
              {
                severity: "error",
                code: "sql",
                message: "exclusion constraints are not supported on partitioned tables",
                span: {
                  start: 44,
                  end: 51,
                  utf16_start: 44,
                  utf16_end: 51,
                  from: { line: 3, column: 3 },
                  to: { line: 3, column: 10 },
                },
              },
            ],
          })
        : json(200, {
            pg_major: major,
            against: 3,
            against_major: 17,
            compiles: true,
            sql: null,
            diagnostics: [],
            changes: { against: 3, summary: "no changes", unchanged: true },
          });
    }
    return json(200, { ...database, pg_major: major });
  });
  return calls;
}

function renderCard(initial?: 15 | 16 | 18) {
  return render(
    <QueryClientProvider client={new QueryClient()}>
      <TooltipProvider>
        <PostgresCard org="acme" database={database} initial={initial} />
      </TooltipProvider>
    </QueryClientProvider>,
  );
}

afterEach(() => vi.unstubAllGlobals());

describe("PostgresCard", () => {
  it("checks a major as soon as it is picked, and keeps the switch shut when it does not compile", async () => {
    stubApi();
    renderCard(15);
    expect(await screen.findByText("v3 does not compile on Postgres 15")).toBeInTheDocument();
    const errors = screen.getByRole("list", { name: "Errors on Postgres 15" });
    expect(within(errors).getByText(/not supported on partitioned tables/)).toBeInTheDocument();
    // The line it is about.
    expect(within(errors).getByText("EXCLUDE USING gist (room WITH =)")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Switch to Postgres 15" })).toBeDisabled();
  });

  it("switches once the latest version compiles there", async () => {
    const calls = stubApi();
    const user = userEvent.setup();
    renderCard(18);
    expect(await screen.findByText("v3 compiles on Postgres 18")).toBeInTheDocument();
    expect(screen.getByText(/reads the same there/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Switch to Postgres 18" }));
    await vi.waitFor(() =>
      expect(calls).toContainEqual({
        method: "PUT",
        path: "/api/orgs/acme/databases/rooms/major",
        body: { pg_major: 18 },
      }),
    );
  });

  it("checks nothing while the current major is picked", () => {
    const calls = stubApi();
    renderCard();
    expect(screen.queryByRole("button", { name: /Switch to/ })).not.toBeInTheDocument();
    expect(calls.filter((c) => c.path.endsWith("/check"))).toEqual([]);
  });
});
