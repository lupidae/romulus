import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { Field } from "./field";
import { PasswordInput } from "./password-input";

describe("PasswordInput", () => {
  it("sits in a Field like any input, its label and errors on the input itself", () => {
    render(
      <Field label="Password" error="Use at least 12 characters.">
        <PasswordInput autoComplete="new-password" />
      </Field>,
    );
    const input = screen.getByLabelText("Password", { selector: "input" });
    expect(input).toHaveAttribute("type", "password");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(input).toHaveAccessibleDescription("Use at least 12 characters.");
  });

  it("shows and hides what was typed, saying which", async () => {
    const user = userEvent.setup();
    render(
      <Field label="Password">
        <PasswordInput defaultValue="violet tractor mango" />
      </Field>,
    );
    const input = screen.getByLabelText("Password", { selector: "input" });
    const toggle = screen.getByRole("button", { name: "Show password" });
    expect(toggle).toHaveAttribute("aria-pressed", "false");
    await user.click(toggle);
    expect(input).toHaveAttribute("type", "text");
    expect(screen.getByRole("button", { name: "Hide password" })).toHaveAttribute(
      "aria-pressed",
      "true",
    );
    await user.click(screen.getByRole("button", { name: "Hide password" }));
    expect(input).toHaveAttribute("type", "password");
  });
});
