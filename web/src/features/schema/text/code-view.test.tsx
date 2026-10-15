import { EditorView } from "@codemirror/view";
import { render, waitFor } from "@testing-library/react";

import type { Editor } from "./codemirror";
import { CodeView, type CodeViewProps } from "./code-view";

function setup(props: Partial<CodeViewProps>) {
  let editor: Editor | null = null;
  const base: CodeViewProps = {
    doc: "CREATE TABLE a (x int);",
    language: "sql",
    label: "Schema as SQL",
    readOnly: true,
    active: true,
    onEditor: (e) => {
      if (e) editor = e;
    },
    ...props,
  };
  const view = render(<CodeView {...base} />);
  return {
    rerender: (next: Partial<CodeViewProps>) => view.rerender(<CodeView {...base} {...next} />),
    text: () => editor?.view.state.doc.toString(),
    editor: () => editor,
  };
}

describe("CodeView", () => {
  test("follows its doc when it owns the text", async () => {
    const view = setup({});
    await waitFor(() => expect(view.text()).toBe("CREATE TABLE a (x int);"));
    view.rerender({ doc: "CREATE TABLE b (y int);" });
    await waitFor(() => expect(view.text()).toBe("CREATE TABLE b (y int);"));
  });

  test("leaves the text to a binding: a lagging doc never replaces it", async () => {
    const binding = [EditorView.theme({})];
    const view = setup({ extensions: binding });
    await waitFor(() => expect(view.text()).toBe("CREATE TABLE a (x int);"));
    view.rerender({ doc: "stale", extensions: binding });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(view.text()).toBe("CREATE TABLE a (x int);");
  });

  test("keeps the first extensions, warns about a new array, and stays the same editor", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const view = setup({ extensions: [EditorView.theme({})] });
    await waitFor(() => expect(view.editor()).not.toBeNull());
    const first = view.editor();
    view.rerender({ extensions: [EditorView.theme({})] });
    await waitFor(() =>
      expect(warn).toHaveBeenCalledWith(expect.stringContaining("`extensions` changed")),
    );
    expect(view.editor()).toBe(first);
    warn.mockRestore();
  });

  test("switches read-only in place", async () => {
    const view = setup({ readOnly: true });
    await waitFor(() => expect(view.editor()).not.toBeNull());
    const first = view.editor()!;
    expect(first.view.state.readOnly).toBe(true);
    view.rerender({ readOnly: false });
    await waitFor(() => expect(first.view.state.readOnly).toBe(false));
    expect(view.editor()).toBe(first);
  });
});
