import { fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";

import { DEFAULT_RATIO, MAX_RATIO, MIN_RATIO, Split } from "./split";

function Harness({
  initial = DEFAULT_RATIO,
  show,
}: {
  initial?: number;
  show?: "both" | "start" | "end";
}) {
  const [ratio, setRatio] = useState(initial);
  return (
    <Split
      ratio={ratio}
      show={show}
      onRatioChange={setRatio}
      label="Resize the panes"
      start={<p>graph</p>}
      end={<p>text</p>}
    />
  );
}

describe("Split", () => {
  test("is a window splitter: focusable, valued, moved with the arrow keys", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const handle = screen.getByRole("separator", { name: "Resize the panes" });
    expect(handle).toHaveAttribute("aria-valuenow", "62");
    await user.tab();
    expect(handle).toHaveFocus();
    await user.keyboard("{ArrowRight}");
    expect(handle).toHaveAttribute("aria-valuenow", "65");
    await user.keyboard("{Shift>}{ArrowLeft}{/Shift}");
    expect(handle).toHaveAttribute("aria-valuenow", "55");
  });

  test("Home and End go to the limits, Enter and double-click reset", async () => {
    const user = userEvent.setup();
    render(<Harness initial={0.5} />);
    const handle = screen.getByRole("separator");
    handle.focus();
    await user.keyboard("{Home}");
    expect(handle).toHaveAttribute("aria-valuenow", String(MIN_RATIO * 100));
    await user.keyboard("{End}");
    expect(handle).toHaveAttribute("aria-valuenow", String(MAX_RATIO * 100));
    await user.keyboard("{Enter}");
    expect(handle).toHaveAttribute("aria-valuenow", String(DEFAULT_RATIO * 100));
    await user.keyboard("{Home}");
    fireEvent.doubleClick(handle);
    expect(handle).toHaveAttribute("aria-valuenow", String(DEFAULT_RATIO * 100));
  });

  test("hiding a pane keeps it mounted and drops the handle", () => {
    render(<Harness show="start" />);
    expect(screen.getByText("text")).toBeInTheDocument();
    expect(screen.getByText("text")).not.toBeVisible();
    expect(screen.queryByRole("separator")).not.toBeInTheDocument();
  });
});
