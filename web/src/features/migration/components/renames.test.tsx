import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import * as React from "react";
import { vi } from "vitest";

import type { RenameCandidate, RenameDecisions } from "../queries";
import { Renames } from "./renames";

// A wide screen (a phone shows the list on demand).
beforeAll(() => {
  vi.stubGlobal("matchMedia", (query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
});

function candidate(
  from: string,
  to: string,
  extra: Partial<RenameCandidate> = {},
): RenameCandidate {
  const rename = { kind: "column" as const, schema: "public", parent: "users", from, to };
  return {
    key: `column/public/users/${from}/${to}`,
    rename,
    confidence: "high",
    score: 90,
    applied: true,
    follows: null,
    rivals: [],
    reason: "Same type, nullability, default and keys; same place; similar name.",
    ...extra,
  };
}

function Harness({
  candidates,
  onChange,
}: {
  candidates: RenameCandidate[];
  onChange?: (patch: RenameDecisions, defaults: RenameDecisions) => void;
}) {
  const [decisions, setDecisions] = React.useState<RenameDecisions>({});
  return (
    <Renames
      candidates={candidates}
      decisions={decisions}
      onChange={(patch, defaults) => {
        onChange?.(patch, defaults);
        setDecisions((d) => ({ ...d, ...patch }));
      }}
    />
  );
}

const unsure = (from: string, to: string, extra: Partial<RenameCandidate> = {}) =>
  candidate(from, to, {
    confidence: "medium",
    applied: false,
    reason: "Same type, nullability, default and keys.",
    ...extra,
  });

describe("Renames", () => {
  it("asks about each detected rename with a switch, on when confident", () => {
    render(
      <Harness
        candidates={[
          candidate("email", "email_address"),
          candidate("legacy", "nickname", {
            confidence: "medium",
            applied: false,
            reason: "Same type, nullability, default and keys.",
          }),
        ]}
      />,
    );
    const confident = screen.getByRole("switch", { name: "Renamed email to email_address?" });
    expect(confident).toBeChecked();
    expect(confident).toHaveAccessibleDescription(
      "Same type, nullability, default and keys; same place; similar name.",
    );
    expect(screen.getByRole("switch", { name: "Renamed legacy to nickname?" })).not.toBeChecked();
    expect(screen.getByText("Unsure")).toBeInTheDocument();
    expect(screen.getByText("1 of 2 on")).toBeInTheDocument();
    // What happens without it, in words.
    expect(
      screen.getByText(/legacy is dropped and nickname added, losing the column's data/),
    ).toBeInTheDocument();
  });

  it("turns one on and its rival off in one change", async () => {
    const onChange = vi.fn();
    const b = candidate("a", "b", {
      confidence: "medium",
      applied: false,
      rivals: ["column/public/users/a/c"],
    });
    const c = candidate("a", "c", {
      confidence: "medium",
      applied: false,
      rivals: ["column/public/users/a/b"],
    });
    render(<Harness candidates={[b, c]} onChange={onChange} />);
    expect(screen.getAllByText("Pick one")).toHaveLength(2);
    await userEvent.click(screen.getByRole("switch", { name: "Renamed a to b?" }));
    expect(onChange).toHaveBeenLastCalledWith(
      { "column/public/users/a/b": true, "column/public/users/a/c": false },
      // Neither is on by itself: switched back, a decision can go.
      { "column/public/users/a/b": false, "column/public/users/a/c": false },
    );
    expect(screen.getByRole("switch", { name: "Renamed a to b?" })).toBeChecked();
    expect(screen.getByRole("switch", { name: "Renamed a to c?" })).not.toBeChecked();
  });

  it("states known renames without a switch, and what comes along with each", () => {
    const table = {
      ...candidate("users", "accounts", { confidence: "certain" }),
      key: "table/public//users/accounts",
      rename: {
        kind: "table" as const,
        schema: "public",
        parent: null,
        from: "users",
        to: "accounts",
      },
    };
    const pkey = {
      ...candidate("users_pkey", "accounts_pkey"),
      key: "constraint/public/accounts/users_pkey/accounts_pkey",
      follows: table.key,
    };
    render(<Harness candidates={[table, pkey]} />);
    expect(screen.queryByRole("switch")).not.toBeInTheDocument();
    const section = screen.getByRole("region", { name: "Renames" });
    expect(within(section).getByText("Known", { selector: "span, div" })).toBeInTheDocument();
    expect(within(section).getByText("users", { selector: "code" })).toBeInTheDocument();
    expect(section).not.toHaveTextContent("?");
    expect(section).toHaveTextContent("Along with it: users_pkey → accounts_pkey");
  });

  it("is read-only when the person may not choose, saying why", () => {
    render(
      <Renames
        candidates={[candidate("email", "email_address")]}
        decisions={{}}
        readOnly="Developers and admins switch renames here."
      />,
    );
    expect(screen.getByRole("switch")).toBeDisabled();
    expect(screen.getByText("Developers and admins switch renames here.")).toBeInTheDocument();
  });

  it("lists unsure rows first, and never moves a row when it is switched", async () => {
    const onChange = vi.fn();
    render(
      <Harness
        candidates={[candidate("email", "email_address"), unsure("legacy", "nickname")]}
        onChange={onChange}
      />,
    );
    const order = screen.getAllByRole("switch").map((s) => s.closest("li")!.textContent);
    expect(order[0]).toContain("legacy");
    expect(screen.getByText("1 of 2 on")).toBeInTheDocument();
    expect(screen.getByText(/1 drops data/)).toHaveClass("text-destructive");
    await userEvent.click(screen.getByRole("switch", { name: "Renamed email to email_address?" }));
    expect(onChange).toHaveBeenLastCalledWith(
      { "column/public/users/email/email_address": false },
      { "column/public/users/email/email_address": true },
    );
    expect(screen.getAllByRole("switch").map((s) => s.closest("li")!.textContent)[0]).toContain(
      "legacy",
    );
    expect(
      screen.getByRole("switch", { name: "Renamed email to email_address?" }),
    ).not.toBeChecked();
    expect(screen.getByText(/2 drop data/)).toBeInTheDocument();
    expect(screen.getByText(/0 of 2 on/)).toBeInTheDocument();
  });

  it("describes a switch that is off with what turning it off does", () => {
    render(<Harness candidates={[unsure("legacy", "nickname")]} />);
    expect(
      screen.getByRole("switch", { name: "Renamed legacy to nickname?" }),
    ).toHaveAccessibleDescription(
      "Same type, nullability, default and keys. Off: legacy is dropped and nickname added, losing the column's data.",
    );
  });

  it("shows what ran once the migration is of the switches on screen", () => {
    // Two tabs turned on rivals at once: one ran.
    const b = unsure("a", "b", { rivals: ["column/public/users/a/c"], applied: true });
    const c = unsure("a", "c", { rivals: ["column/public/users/a/b"] });
    const decisions = { "column/public/users/a/b": true, "column/public/users/a/c": true };
    const { rerender } = render(<Renames candidates={[b, c]} decisions={decisions} />);
    expect(screen.getByRole("switch", { name: "Renamed a to c?" })).toBeChecked();
    rerender(<Renames candidates={[b, c]} decisions={decisions} fresh />);
    expect(screen.getByRole("switch", { name: "Renamed a to b?" })).toBeChecked();
    expect(screen.getByRole("switch", { name: "Renamed a to c?" })).not.toBeChecked();
  });

  it("says a move to another schema is one", () => {
    const moved = {
      ...candidate("audit", "audit"),
      key: "table/public//audit/audit/archive",
      rename: {
        kind: "table" as const,
        schema: "public",
        parent: null,
        from: "audit",
        to: "audit",
        to_schema: "archive",
      },
    };
    render(<Harness candidates={[moved]} />);
    const toggle = screen.getByRole("switch", { name: "Moved public.audit to archive?" });
    expect(toggle).toBeChecked();
  });

  it("on a phone, puts what drops data first and never behind Show, wherever it is", () => {
    vi.stubGlobal("matchMedia", (query: string) => ({
      matches: true,
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
    }));
    try {
      const email = candidate("email", "email_address");
      const nickname = unsure("legacy", "nickname");
      const tag = candidate("tag", "label");
      // A coworker switched the confident email rename off.
      const off = { "column/public/users/email/email_address": false };
      const { rerender } = render(<Renames candidates={[nickname, tag, email]} decisions={off} />);
      const names = () =>
        screen.getAllByRole("switch").map((s) => s.closest("li")!.textContent.split("?")[0]);
      // Both off and dropping data, so both lead, before the confident one.
      expect(names()[0]).toContain("legacy");
      expect(names()[1]).toContain("email");
      // The confident, on one waits behind Show.
      expect(names()).toHaveLength(2);
      expect(screen.getByRole("button", { name: "Show 1 more" })).toBeInTheDocument();
      // Switched off later, by someone: shown at once, in its place (last).
      rerender(
        <Renames
          candidates={[nickname, tag, email]}
          decisions={{ ...off, "column/public/users/tag/label": false }}
        />,
      );
      expect(names()).toHaveLength(3);
      expect(names()[2]).toContain("tag");
    } finally {
      vi.stubGlobal("matchMedia", (query: string) => ({
        matches: false,
        media: query,
        addEventListener: () => {},
        removeEventListener: () => {},
      }));
    }
  });

  it("shows nothing without renames", () => {
    const { container } = render(<Renames candidates={[]} decisions={{}} />);
    expect(container).toBeEmptyDOMElement();
  });
});
