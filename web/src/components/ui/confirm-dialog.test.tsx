import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import * as React from "react";
import { vi } from "vitest";

import { ConfirmDialog } from "./confirm-dialog";

function Harness({ onConfirm }: { onConfirm: () => unknown }) {
  const [open, setOpen] = React.useState(true);
  return (
    <ConfirmDialog
      open={open}
      onOpenChange={setOpen}
      title="Remove Bob from Acme?"
      description="They lose access right away."
      confirmLabel="Remove member"
      destructive
      onConfirm={onConfirm}
    />
  );
}

describe("ConfirmDialog", () => {
  it("is an alert dialog that starts on Cancel, so Enter never confirms by accident", () => {
    render(<Harness onConfirm={() => undefined} />);
    const dialog = screen.getByRole("alertdialog", { name: "Remove Bob from Acme?" });
    expect(dialog).toHaveAccessibleDescription("They lose access right away.");
    expect(screen.getByRole("button", { name: "Cancel" })).toHaveFocus();
  });

  it("closes once the action succeeded", async () => {
    const onConfirm = vi.fn(() => Promise.resolve());
    render(<Harness onConfirm={onConfirm} />);
    await userEvent.click(screen.getByRole("button", { name: "Remove member" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(onConfirm).toHaveBeenCalledOnce();
  });

  it("stays open for another try when the action fails", async () => {
    render(<Harness onConfirm={() => Promise.reject(new Error("409"))} />);
    const confirm = screen.getByRole("button", { name: "Remove member" });
    await userEvent.click(confirm);
    await waitFor(() => expect(confirm).not.toHaveAttribute("aria-busy"));
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();
  });
});
