import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { Diagnostic } from "../queries";
import type { CompileView } from "../session";
import { CompileStatus } from "./compile-status";

const error: Diagnostic = {
  code: "sql",
  severity: "error",
  message: 'syntax error at or near ")"',
  hint: null,
  span: {
    start: 40,
    end: 41,
    utf16_start: 40,
    utf16_end: 41,
    from: { line: 3, column: 7 },
    to: { line: 3, column: 8 },
  },
};

const view = (patch: Partial<CompileView>): CompileView => ({
  state: "ok",
  stale: false,
  diagnostics: [],
  model: null,
  sha256: null,
  ...patch,
});

describe("CompileStatus", () => {
  it("says where the SQL stands", () => {
    const { rerender } = render(<CompileStatus compile={view({})} onJump={() => {}} />);
    expect(screen.getByTestId("compile-status")).toHaveTextContent("Compiled");
    rerender(<CompileStatus compile={view({ stale: true })} onJump={() => {}} />);
    expect(screen.getByTestId("compile-status")).toHaveTextContent("Compiling…");
    rerender(
      <CompileStatus
        compile={view({ state: "failed", diagnostics: [error, error] })}
        onJump={() => {}}
      />,
    );
    expect(screen.getByTestId("compile-status")).toHaveTextContent("2 errors");
    // Conflict blocks from "Update draft" are called what they are.
    rerender(
      <CompileStatus
        compile={view({
          state: "failed",
          diagnostics: [
            { ...error, code: "conflict", message: "Conflict with v2: keep yours or theirs." },
          ],
        })}
        onJump={() => {}}
      />,
    );
    expect(screen.getByTestId("compile-status")).toHaveTextContent("1 conflict");
    rerender(<CompileStatus compile={view({ state: "unavailable" })} onJump={() => {}} />);
    expect(screen.getByTestId("compile-status")).toHaveTextContent("Not checked");
  });

  it("says, checked by the parser, what the preview leaves out", async () => {
    const note: Diagnostic = {
      ...error,
      code: "not_run",
      severity: "info",
      message: "CREATE STATISTICS is not supported in the browser preview.",
    };
    const { rerender } = render(
      <CompileStatus compile={view({})} checker="parser" onJump={() => {}} />,
    );
    expect(screen.getByTestId("compile-status")).toHaveTextContent("Checked");
    rerender(
      <CompileStatus compile={view({ diagnostics: [note] })} checker="parser" onJump={() => {}} />,
    );
    expect(screen.getByTestId("compile-status")).toHaveTextContent("Checked, 1 note");
    await userEvent.click(screen.getByTestId("compile-status"));
    expect(screen.getByText(/except what is below/)).toBeInTheDocument();
    expect(screen.getByText(note.message)).toBeInTheDocument();
  });

  it("goes to an error from its list", async () => {
    const onJump = vi.fn();
    render(
      <CompileStatus compile={view({ state: "failed", diagnostics: [error] })} onJump={onJump} />,
    );
    await userEvent.click(screen.getByTestId("compile-status"));
    await userEvent.click(await screen.findByRole("button", { name: /syntax error/ }));
    expect(screen.getByText("Line 3, column 7")).toBeInTheDocument();
    expect(onJump).toHaveBeenCalledWith(error);
  });
});
