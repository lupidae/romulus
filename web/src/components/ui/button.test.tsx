import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { Button } from "./button";

describe("Button", () => {
  it("is a type=button by default, so it never submits a form by accident", () => {
    render(<Button>Save</Button>);
    expect(screen.getByRole("button", { name: "Save" })).toHaveAttribute("type", "button");
  });

  it("keeps its label (and width) while loading but ignores clicks", async () => {
    const onClick = vi.fn();
    render(
      <Button loading onClick={onClick}>
        Publish v13
      </Button>,
    );
    const button = screen.getByRole("button", { name: "Publish v13" });
    expect(button).toHaveAttribute("aria-busy", "true");
    // Visibility would drop the label from the accessibility tree; opacity keeps it.
    expect(screen.getByText("Publish v13")).toHaveClass("opacity-0");
    expect(screen.getByText("Publish v13")).not.toHaveClass("invisible");
    expect(button).not.toBeDisabled();
    await userEvent.click(button);
    expect(onClick).not.toHaveBeenCalled();
  });

  it("renders its child when asChild, for links styled as buttons", () => {
    render(
      <Button asChild variant="primary">
        <a href="/api/auth/login">Continue</a>
      </Button>,
    );
    expect(screen.getByRole("link", { name: "Continue" })).toHaveAttribute(
      "href",
      "/api/auth/login",
    );
  });
});
