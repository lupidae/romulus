import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { HeroBoundary } from "./boundary";

function Broken(): never {
  throw new Error("broken");
}

describe("HeroBoundary", () => {
  it("draws nothing and reports a render that throws", () => {
    const onError = vi.fn();
    // React logs the error it caught: expected here.
    const quiet = vi.spyOn(console, "error").mockImplementation(() => {});
    const { container } = render(
      <HeroBoundary onError={onError}>
        <p>before</p>
        <Broken />
      </HeroBoundary>,
    );
    quiet.mockRestore();
    expect(container).toBeEmptyDOMElement();
    expect(onError).toHaveBeenCalledWith(expect.objectContaining({ message: "broken" }));
  });

  it("draws its children otherwise", () => {
    const { getByText } = render(
      <HeroBoundary onError={() => {}}>
        <p>fine</p>
      </HeroBoundary>,
    );
    expect(getByText("fine")).toBeInTheDocument();
  });
});
