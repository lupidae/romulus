import { render, screen } from "@testing-library/react";

import { Field } from "./field";
import { Input } from "./input";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "./select";

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
      <Field
        label="Email"
        description="Work address."
        error="Enter an address like name@company.com."
      >
        <Input />
      </Field>,
    );
    const input = screen.getByLabelText("Email");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(input).toHaveAccessibleDescription("Enter an address like name@company.com.");
    expect(screen.queryByText("Work address.")).not.toBeInTheDocument();
  });

  it("labels, describes and marks a select's trigger, the root having no element", () => {
    render(
      <Field label="Access" error="Pick who may push.">
        <Select defaultValue="push">
          <SelectTrigger>
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            <SelectItem value="push">Push</SelectItem>
          </SelectContent>
        </Select>
      </Field>,
    );
    const trigger = screen.getByRole("combobox", { name: "Access" });
    expect(trigger).toHaveAccessibleDescription("Pick who may push.");
    expect(trigger).toHaveAttribute("aria-invalid", "true");
  });
});
