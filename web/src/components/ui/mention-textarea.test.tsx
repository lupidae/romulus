import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import * as React from "react";
import { describe, expect, it } from "vitest";

import { MentionTextarea } from "./mention-textarea";

function Reply() {
  const [value, setValue] = React.useState("");
  return (
    <MentionTextarea
      aria-label="Reply"
      value={value}
      onValueChange={setValue}
      people={[{ id: "u-alice", name: "Alice" }]}
    />
  );
}

const frame = () => new Promise((resolve) => requestAnimationFrame(resolve));

describe("a mention picked", () => {
  it("leaves the caret after the name for whatever is typed next, however slowly", async () => {
    render(<Reply />);
    const area = screen.getByRole("combobox", { name: "Reply" });
    await userEvent.type(area, "Agreed. @Al");
    await userEvent.click(screen.getByRole("option", { name: /Alice/ }));
    await userEvent.keyboard("we ");
    await frame();
    await userEvent.keyboard("have duplicates");
    expect(area).toHaveValue("Agreed. @Alice we have duplicates");
  });
});
