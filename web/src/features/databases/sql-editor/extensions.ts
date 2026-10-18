// The upload editor's CodeMirror extensions, loaded with the editor (this
// module imports CodeMirror, so only the upload dialog pulls it in, lazily).

import { history, historyKeymap } from "@codemirror/commands";
import { EditorSelection, type Extension, Prec, StateEffect, StateField } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, keymap, placeholder } from "@codemirror/view";

/** A range to mark as wrong, in UTF-16 offsets (what CodeMirror counts in). */
export interface Mark {
  from: number;
  to: number;
  message: string;
}

const setMarks = StateEffect.define<Mark[]>();

function decorations(
  view: { doc: { length: number; lineAt(pos: number): { from: number } } },
  marks: Mark[],
) {
  const length = view.doc.length;
  const ranges = [];
  for (const mark of marks) {
    let from = Math.min(Math.max(mark.from, 0), length);
    let to = Math.min(Math.max(mark.to, from), length);
    // An empty range (an error at the end of a statement) still needs a
    // character to underline.
    if (to === from) {
      if (to < length) to += 1;
      else if (from > 0) from -= 1;
    }
    ranges.push(Decoration.line({ class: "cm-error-line" }).range(view.doc.lineAt(from).from));
    if (to > from) {
      ranges.push(
        Decoration.mark({ class: "cm-error-mark", attributes: { title: mark.message } }).range(
          from,
          to,
        ),
      );
    }
  }
  return Decoration.set(ranges, true);
}

const marksField = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(deco, tr) {
    for (const effect of tr.effects) {
      if (effect.is(setMarks)) return decorations(tr.state, effect.value);
    }
    return tr.docChanged ? deco.map(tr.changes) : deco;
  },
  provide: (field) => EditorView.decorations.from(field),
});

const theme = EditorView.theme({
  ".cm-error-mark": {
    textDecoration: "underline wavy var(--color-destructive)",
    textDecorationSkipInk: "none",
    textUnderlineOffset: "0.1875rem",
    backgroundColor: "color-mix(in oklab, var(--color-destructive) 10%, transparent)",
  },
  ".cm-error-line": {
    backgroundColor: "color-mix(in oklab, var(--color-destructive) 5%, transparent)",
  },
  ".cm-placeholder": { color: "var(--color-faint-foreground)", fontStyle: "normal" },
});

export interface UploadEditorOptions {
  placeholder: string;
  /** Every change of the text. */
  onChange: (doc: string) => void;
  /** ⌘↵ / Ctrl+↵. */
  onSubmit: () => void;
}

/** Undo, ⌘↵ to submit, a placeholder, and the error marks. */
export function uploadEditorExtensions(options: UploadEditorOptions): Extension[] {
  return [
    history(),
    keymap.of(historyKeymap),
    // Above the default keymap, which inserts a blank line on Mod-Enter.
    Prec.highest(
      keymap.of([
        {
          key: "Mod-Enter",
          run: () => {
            options.onSubmit();
            return true;
          },
        },
      ]),
    ),
    placeholder(options.placeholder),
    marksField,
    theme,
    EditorView.updateListener.of((update) => {
      if (update.docChanged) options.onChange(update.state.doc.toString());
    }),
  ];
}

/** Underlines `marks` (replacing the previous ones). */
export function showMarks(view: EditorView, marks: Mark[]) {
  view.dispatch({ effects: setMarks.of(marks) });
}

/** Replaces the whole text (a dropped file), cursor at the top. */
export function replaceText(view: EditorView, text: string) {
  view.dispatch({
    changes: { from: 0, to: view.state.doc.length, insert: text },
    selection: { anchor: 0 },
    effects: [setMarks.of([]), EditorView.scrollIntoView(0)],
  });
}

/** Selects a range, scrolls it to the middle and focuses the editor. */
export function jumpTo(view: EditorView, from: number, to: number) {
  const length = view.state.doc.length;
  const range = EditorSelection.range(Math.min(from, length), Math.min(Math.max(to, from), length));
  view.dispatch({
    selection: range,
    effects: EditorView.scrollIntoView(range, { y: "center" }),
  });
  view.focus();
}
