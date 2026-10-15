// The SQL editor of a draft: CodeMirror bound to the doc's `sql` Y.Text
// (y-codemirror.next), with everyone's carets and selections, one undo
// history shared with the graph (the session's Y.UndoManager), and the
// compile's diagnostics. Loaded with the editor chunk.

import { type Diagnostic as CmDiagnostic, lintGutter, setDiagnostics } from "@codemirror/lint";
import { type Extension, Prec } from "@codemirror/state";
import { EditorView, keymap, ViewPlugin } from "@codemirror/view";
import {
  yCollab,
  ySyncAnnotation,
  type YSyncConfig,
  ySyncFacet,
  yUndoManagerKeymap,
} from "y-codemirror.next";

import type * as Y from "yjs";

import type { Diagnostic } from "./queries";
import type { DraftSession } from "./session";

const theme = EditorView.theme({
  // Collaborators' carets: their presence color (the server stamps it as a
  // token), the name always shown, small, above the line.
  ".cm-ySelectionCaret": {
    borderLeftWidth: "0.125rem",
    borderRightWidth: "0",
    marginLeft: "-0.0625rem",
  },
  ".cm-ySelectionCaretDot": { display: "none" },
  ".cm-ySelectionInfo": {
    top: "-1.15rem",
    left: "-0.125rem",
    padding: "0.0625rem 0.3125rem",
    borderRadius: "0.25rem 0.25rem 0.25rem 0",
    fontFamily: "var(--font-sans)",
    fontSize: "0.6875rem",
    fontWeight: "500",
    lineHeight: "1rem",
    color: "var(--color-presence-foreground)",
    opacity: "1",
    pointerEvents: "none",
  },
  ".cm-ySelection": { borderRadius: "0.125rem" },
  // Diagnostics, in the kit's terms.
  ".cm-lintRange-error": {
    backgroundImage: "none",
    textDecoration: "underline wavy var(--color-destructive)",
    textDecorationSkipInk: "none",
    textUnderlineOffset: "0.1875rem",
  },
  ".cm-lintRange-info": {
    backgroundImage: "none",
    textDecoration: "underline dotted var(--color-info)",
    textUnderlineOffset: "0.1875rem",
  },
  ".cm-gutter-lint": { width: "0.875rem" },
  ".cm-lint-marker": { width: "0.625rem", height: "0.625rem", margin: "0.3125rem 0 0 0.125rem" },
  ".cm-tooltip-lint, .cm-tooltip.cm-tooltip-hover": {
    backgroundColor: "var(--color-popover)",
    color: "var(--color-popover-foreground)",
    border: "0.0625rem solid var(--color-border)",
    borderRadius: "var(--radius-md)",
    boxShadow: "var(--shadow-popover)",
    fontFamily: "var(--font-sans)",
    fontSize: "var(--text-sm)",
    maxWidth: "28rem",
  },
  ".cm-diagnostic": { padding: "0.375rem 0.625rem", borderLeftWidth: "0.1875rem" },
  ".cm-diagnostic-error": { borderLeftColor: "var(--color-destructive)" },
  ".cm-diagnostic-info": { borderLeftColor: "var(--color-info)" },
});

/**
 * The editor's text must be the Y.Text's when the binding starts: CodeMirror
 * is created a tick after the page read the text, and a remote edit can land
 * in between. If so, take the doc's text, marked as coming from Yjs so the
 * binding does not send it back.
 */
const seedGuard = (ytext: Y.Text) =>
  ViewPlugin.define((view) => {
    if (view.state.doc.toString() !== ytext.toJSON()) {
      const conf = view.state.facet<YSyncConfig>(ySyncFacet);
      queueMicrotask(() => {
        const text = ytext.toJSON();
        if (view.state.doc.toString() === text) return;
        view.dispatch({
          changes: { from: 0, to: view.state.doc.length, insert: text },
          annotations: [ySyncAnnotation.of(conf)],
        });
      });
    }
    return {};
  });

/**
 * The extensions of a draft's SQL editor. Create once per session.
 * `onView` hears of the editor view when it exists (and null when it goes):
 * diagnostics and jumps need it.
 */
export function draftExtensions(
  session: DraftSession,
  onView: (view: EditorView | null) => void,
): Extension[] {
  return [
    yCollab(session.sql, session.awareness, { undoManager: session.undo }),
    seedGuard(session.sql),
    Prec.high(keymap.of(yUndoManagerKeymap)),
    lintGutter(),
    theme,
    ViewPlugin.define((view) => {
      onView(view);
      return { destroy: () => onView(null) };
    }),
  ];
}

/** Shows the compile's diagnostics (positions in UTF-16, like CodeMirror's). */
export function showDiagnostics(view: EditorView, diagnostics: Diagnostic[]) {
  const length = view.state.doc.length;
  const found: CmDiagnostic[] = diagnostics.map((d) => {
    const from = Math.min(d.span?.utf16_start ?? 0, length);
    const to = Math.min(Math.max(d.span?.utf16_end ?? from, from + 1), length);
    return {
      from,
      to: Math.max(to, from),
      severity: d.severity === "error" ? "error" : "info",
      message: d.hint ? `${d.message}\n${d.hint}` : d.message,
    };
  });
  view.dispatch(setDiagnostics(view.state, found));
}

/** Puts the caret on a diagnostic and brings it into view. */
export function jumpTo(view: EditorView, diagnostic: Diagnostic) {
  const length = view.state.doc.length;
  const from = Math.min(diagnostic.span?.utf16_start ?? 0, length);
  const to = Math.min(diagnostic.span?.utf16_end ?? from, length);
  view.dispatch({
    selection: { anchor: from, head: Math.max(from, to) },
    effects: EditorView.scrollIntoView(from, { y: "center" }),
  });
  view.focus();
}
