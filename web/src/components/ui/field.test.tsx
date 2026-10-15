import { render, screen } from "@testing-library/react";

import { Field } from "./field";
import { Input } from "./input";

describe("Field", () => {
  it("labels the control and describes it", () => {
    render(
      <Field label="Database name" description="Lowercase, used in links.">
        <Input />
      </Field>,
    );
    const input = screen.getByLabelText("Database name");
    expect(input).toHaveAccessibleDescription("Lowercase, used in links.");
    expect(input).not.toHaveAttribute("aria-invalid");
  });

  it("marks the control invalid and announces the error instead of the description", () => {
    render(
      <Field label="Email" description="Work address." error="Enter an address like name@company.com.">
        <Input />
      </Field>,
    );
    const input = screen.getByLabelText("Email");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(input).toHaveAccessibleDescription("Enter an address like name@company.com.");
    expect(screen.queryByText("Work address.")).not.toBeInTheDocument();
  });
});
