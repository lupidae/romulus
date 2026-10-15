import { render, screen } from "@testing-library/react";

import { Avatar, AvatarStack, initials, presenceIndex } from "./avatar";
import { TooltipProvider } from "./tooltip";

describe("presenceIndex", () => {
  it("is stable for a user and stays within the eight colors", () => {
    for (const id of ["u-alice", "u-bob", "", "0190f3c2-7b5e-7c1a-9d3e-111111111111"]) {
      const index = presenceIndex(id);
      expect(index).toBe(presenceIndex(id));
      expect(index).toBeGreaterThanOrEqual(1);
      expect(index).toBeLessThanOrEqual(8);
    }
  });
});

describe("initials", () => {
  it("takes the first and last word", () => {
    expect(initials("Alice Martin")).toBe("AM");
    expect(initials("Jean-Luc de la Fontaine")).toBe("JF");
    expect(initials("bob")).toBe("B");
    expect(initials("  ")).toBe("?");
  });
});

describe("Avatar", () => {
  it("names the person for screen readers", () => {
    render(<Avatar name="Alice Martin" id="u-alice" />);
    expect(screen.getByText("Alice Martin")).toHaveClass("sr-only");
  });
});

describe("AvatarStack", () => {
  it("collapses everyone past max into +N and says so", () => {
    const people = ["Ann", "Ben", "Cy", "Dee", "Eve", "Fay"].map((name) => ({ id: name, name }));
    render(
      <TooltipProvider>
        <AvatarStack people={people} max={4} />
      </TooltipProvider>,
    );
    expect(screen.getByText("+2")).toBeInTheDocument();
    expect(screen.getByRole("group")).toHaveAccessibleName("Ann, Ben, Cy, Dee and 2 more");
  });
});
