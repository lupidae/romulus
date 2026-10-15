import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, vi } from "vitest";

import { CopyField } from "./copy-field";

const link = "https://romulus.test/app/invite/abc";

afterEach(() => {
  vi.restoreAllMocks();
});

describe("CopyField", () => {
  it("copies the value and says so in place", async () => {
    const user = userEvent.setup();
    const write = vi.spyOn(navigator.clipboard, "writeText");
    render(<CopyField value={link} label="Invitation link" />);
    expect(screen.getByRole("textbox", { name: "Invitation link" })).toHaveValue(link);

    await user.click(screen.getByRole("button", { name: "Copy invitation link" }));
    expect(write).toHaveBeenCalledWith(link);
    expect(screen.getByRole("button", { name: "Invitation link copied" })).toHaveTextContent(
      "Copied",
    );
    expect(screen.getByRole("status")).toHaveTextContent("Invitation link copied to the clipboard");
  });

  it("selects the text when the clipboard refuses, so the keyboard still works", async () => {
    const user = userEvent.setup();
    vi.spyOn(navigator.clipboard, "writeText").mockRejectedValue(new Error("denied"));
    render(<CopyField value={link} label="Invitation link" />);
    await user.click(screen.getByRole("button", { name: "Copy invitation link" }));
    const input = screen.getByRole<HTMLInputElement>("textbox", { name: "Invitation link" });
    expect(input.selectionStart).toBe(0);
    expect(input.selectionEnd).toBe(link.length);
  });
});
