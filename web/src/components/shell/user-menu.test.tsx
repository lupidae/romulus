import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeAll, describe, expect, it, vi } from "vitest";

import { UserMenu } from "./user-menu";

vi.mock("@tanstack/react-router", () => ({ useNavigate: () => vi.fn() }));

const user = { id: "u1", name: "Ada Lovelace", email: "ada@acme.io" };
const commit = "1f1ded1c0ffee5e5a1d2b3c4d5e6f708192a3b4c";

beforeAll(() => {
  // The theme choice follows the system's; jsdom has no media queries.
  vi.stubGlobal("matchMedia", () => ({
    matches: false,
    addEventListener: () => {},
    removeEventListener: () => {},
  }));
});

describe("UserMenu", () => {
  it("links the source of the commit that runs", async () => {
    render(
      <UserMenu
        user={user}
        onSignOut={() => {}}
        source={{
          url: `https://github.com/lupidae/romulus/tree/${commit}`,
          commit,
          version: "0.1.0",
        }}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Account: Ada Lovelace" }));
    const link = screen.getByRole("menuitem", { name: /Source code/ });
    expect(link).toHaveAttribute("href", `https://github.com/lupidae/romulus/tree/${commit}`);
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveTextContent("1f1ded1");
  });

  it("names the version when the build did not say which commit", async () => {
    render(
      <UserMenu
        user={user}
        onSignOut={() => {}}
        source={{ url: "https://git.example.com/acme/romulus", commit: null, version: "0.1.0" }}
      />,
    );
    await userEvent.click(screen.getByRole("button", { name: "Account: Ada Lovelace" }));
    const link = screen.getByRole("menuitem", { name: /Source code/ });
    expect(link).toHaveAttribute("href", "https://git.example.com/acme/romulus");
    expect(link).toHaveTextContent("v0.1.0");
  });
});
