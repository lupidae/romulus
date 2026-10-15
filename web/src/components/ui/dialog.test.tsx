import { render, screen } from "@testing-library/react";
import { afterEach, vi } from "vitest";

import { Dialog, DialogContent, DialogDescription, DialogTitle } from "./dialog";

afterEach(() => {
  vi.restoreAllMocks();
});

function spyConsole() {
  return [
    vi.spyOn(console, "warn").mockImplementation(() => {}),
    vi.spyOn(console, "error").mockImplementation(() => {}),
  ];
}

describe("DialogContent", () => {
  it("is described by its DialogDescription", () => {
    const spies = spyConsole();
    render(
      <Dialog open>
        <DialogContent>
          <DialogTitle>Publish v13</DialogTitle>
          <DialogDescription>Creates version 13 and closes the draft.</DialogDescription>
        </DialogContent>
      </Dialog>,
    );
    const dialog = screen.getByRole("dialog", { name: "Publish v13" });
    expect(dialog).toHaveAccessibleDescription("Creates version 13 and closes the draft.");
    for (const spy of spies) expect(spy).not.toHaveBeenCalled();
  });

  it("accepts an explicit opt-out too", () => {
    const spies = spyConsole();
    render(
      <Dialog open>
        <DialogContent aria-describedby={undefined}>
          <DialogTitle>Rename table</DialogTitle>
        </DialogContent>
      </Dialog>,
    );
    expect(screen.getByRole("dialog", { name: "Rename table" })).not.toHaveAttribute(
      "aria-describedby",
    );
    for (const spy of spies) expect(spy).not.toHaveBeenCalled();
  });

  it("without a description, references none and stays quiet", () => {
    const spies = spyConsole();
    render(
      <Dialog open>
        <DialogContent>
          <DialogTitle>Rename table</DialogTitle>
        </DialogContent>
      </Dialog>,
    );
    // Radix only links a description that is rendered: no dangling id.
    expect(screen.getByRole("dialog", { name: "Rename table" })).not.toHaveAttribute(
      "aria-describedby",
    );
    for (const spy of spies) expect(spy).not.toHaveBeenCalled();
  });
});
