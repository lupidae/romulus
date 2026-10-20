import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type * as React from "react";
import { vi } from "vitest";

import { TooltipProvider } from "@/components/ui/tooltip";

import type { Org, OrgMember } from "../queries";
import { MembersTable } from "./members-table";

vi.mock("@tanstack/react-router", () => ({ useNavigate: () => vi.fn() }));

const member = (id: string, name: string, role: OrgMember["role"]): OrgMember => ({
  user_id: id,
  name,
  email: `${name.toLowerCase()}@acme.test`,
  role,
  joined_at: "2026-10-15T12:00:00Z",
});
const members = [member("a", "Alice", "admin"), member("b", "Bob", "developer")];
const org = (role: Org["role"]): Org => ({
  id: "o",
  slug: "acme",
  name: "Acme",
  role,
  approvals_required: 0,
  require_passing_test: false,
});

function renderTable(ui: React.ReactElement) {
  const client = new QueryClient();
  return render(
    <QueryClientProvider client={client}>
      <TooltipProvider>{ui}</TooltipProvider>
    </QueryClientProvider>,
  );
}

describe("MembersTable", () => {
  it("marks me and lets an admin change roles in place", () => {
    renderTable(<MembersTable org={org("admin")} members={members} myId="a" />);
    const alice = screen.getAllByRole("row")[1]!;
    expect(within(alice).getByText("You")).toBeInTheDocument();
    // One role control per person (phones and wide screens each render theirs).
    expect(screen.getAllByRole("combobox", { name: "Role of Bob" }).length).toBeGreaterThan(0);
  });

  it("explains why the last admin cannot leave instead of hiding the action", async () => {
    const user = userEvent.setup();
    renderTable(<MembersTable org={org("admin")} members={members} myId="a" />);
    await user.click(screen.getByRole("button", { name: "More actions for Alice" }));
    const leave = await screen.findByRole("menuitem", { name: "Leave Acme…" });
    expect(leave).toHaveAttribute("aria-disabled", "true");
    expect(screen.getByText(/You are the only admin/)).toHaveTextContent(
      "Make someone else an admin first.",
    );
  });

  it("shows roles read-only to non-admins, with only their own row's actions", () => {
    renderTable(
      <MembersTable
        org={org("developer")}
        members={[member("a", "Alice", "admin"), member("b", "Bob", "developer")]}
        myId="b"
      />,
    );
    expect(screen.queryByRole("combobox")).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "More actions for Alice" }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "More actions for Bob" })).toBeInTheDocument();
  });
});
