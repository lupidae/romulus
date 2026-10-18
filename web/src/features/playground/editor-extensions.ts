// The console editor's CodeMirror extensions, loaded with CodeMirror (this
// module imports it, so only the playground page pulls it in, lazily): the
// upload editor's (undo, marks, placeholder, ⌘↵) plus ⇧⌘↵ to explain and
// Esc to cancel a run.

import { Prec } from "@codemirror/state";
import { EditorView, keymap } from "@codemirror/view";

import { uploadEditorExtensions } from "@/features/databases/sql-editor/extensions";

export { jumpTo, showMarks } from "@/features/databases/sql-editor/extensions";

export interface ConsoleEditorOptions {
  onChange: (doc: string) => void;
  onRun: () => void;
  onExplain: () => void;
  onCancel: () => void;
}

export function consoleEditorExtensions(options: ConsoleEditorOptions) {
  return [
    // Queries are read whole: long lines wrap.
    EditorView.lineWrapping,
    ...uploadEditorExtensions({
      placeholder: "SELECT * FROM … LIMIT 100;",
      onChange: options.onChange,
      onSubmit: options.onRun,
    }),
    Prec.highest(
      keymap.of([
        {
          key: "Shift-Mod-Enter",
          run: () => {
            options.onExplain();
            return true;
          },
        },
        {
          key: "Escape",
          run: () => {
            options.onCancel();
            return false;
          },
        },
      ]),
    ),
  ];
}
