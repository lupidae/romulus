// The SQL editor of a draft: CodeMirror bound to the doc's `sql` Y.Text
// (y-codemirror.next), with everyone's carets and selections, one undo
// history shared with the graph (the session's Y.UndoManager), the
// compile's diagnostics, and "Update draft"'s conflict blocks with their
// buttons. Loaded with the editor chunk.

import { type Diagnostic as CmDiagnostic, lintGutter, setDiagnostics } from "@codemirror/lint";
import {
  type EditorState,
  type Extension,
  Prec,
  type Range,
  StateEffect,
  StateField,
} from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  EditorView,
  keymap,
  placeholder,
  ViewPlugin,
  WidgetType,
} from "@codemirror/view";
import {
  yCollab,
  ySyncAnnotation,
  type YSyncConfig,
  ySyncFacet,
  yUndoManagerKeymap,
} from "y-codemirror.next";

import type * as Y from "yjs";

import { buttonVariants } from "@/components/ui/button";

import { type Choice, type ConflictBlock, findConflicts, resolution } from "./conflicts";
import type { Diagnostic } from "./queries";
import type { DraftSession } from "./session";

const theme = EditorView.theme({
  ".cm-placeholder": { color: "var(--color-faint-foreground)", fontStyle: "italic" },
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
  ".cm-diagnostic": { padding: "0.375rem 0.625rem", borderLeft: "none", whiteSpace: "pre-wrap" },
  // SQL a graph edit just wrote: lit, then fading back.
  ".cm-graphEdit": {
    backgroundColor: "color-mix(in oklab, var(--color-primary) 16%, transparent)",
    borderRadius: "0.125rem",
    animation: "cm-graph-edit 2.4s ease-out forwards",
  },
  "@keyframes cm-graph-edit": {
    "0%, 50%": { backgroundColor: "color-mix(in oklab, var(--color-primary) 16%, transparent)" },
    "100%": { backgroundColor: "transparent" },
  },
  // Conflict blocks: a bar above each, yours and theirs tinted apart, the
  // marker lines quiet.
  ".cm-conflict-bar": {
    display: "flex",
    flexWrap: "wrap",
    alignItems: "center",
    gap: "0.375rem",
    margin: "0.375rem 0 0.125rem",
    padding: "0.375rem 0.5rem",
    border: "0.0625rem solid color-mix(in oklab, var(--color-warning) 40%, transparent)",
    borderRadius: "var(--radius-md)",
    backgroundColor: "color-mix(in oklab, var(--color-warning) 10%, var(--color-card))",
    fontFamily: "var(--font-sans)",
    fontSize: "var(--text-sm)",
    lineHeight: "1.25rem",
    whiteSpace: "normal",
  },
  ".cm-conflict-title": { fontWeight: "600", color: "var(--color-foreground)" },
  ".cm-conflict-hint": { color: "var(--color-muted-foreground)", marginRight: "auto" },
  ".cm-conflict-marker": { color: "var(--color-faint-foreground)" },
  ".cm-conflict-yours": {
    backgroundColor: "color-mix(in oklab, var(--color-info) 9%, transparent)",
  },
  ".cm-conflict-theirs": {
    backgroundColor: "color-mix(in oklab, var(--color-success) 9%, transparent)",
  },
});

const flash = StateEffect.define<{ from: number; to: number } | null>();
/** The latest flash per editor: a timer only clears its own. */
const flashes = new WeakMap<EditorView, number>();
const graphEditMark = Decoration.mark({ class: "cm-graphEdit" });

/** Where SQL written from the graph is lit for a moment (mapped through later edits). */
const flashed = StateField.define<DecorationSet>({
  create: () => Decoration.none,
  update(marks, tr) {
    let next = marks.map(tr.changes);
    for (const effect of tr.effects) {
      if (!effect.is(flash)) continue;
      next = effect.value
        ? Decoration.set([graphEditMark.range(effect.value.from, effect.value.to)])
        : Decoration.none;
    }
    return next;
  },
  provide: (field) => EditorView.decorations.from(field),
});

const CHOICES: { choice: Choice; label: string }[] = [
  { choice: "yours", label: "Keep yours" },
  { choice: "theirs", label: "Keep theirs" },
  { choice: "both", label: "Keep both" },
];

/** The bar above a conflict block: what it is, and how to resolve it. */
class ConflictBar extends WidgetType {
  readonly block: ConflictBlock;
  readonly readOnly: boolean;

  constructor(block: ConflictBlock, readOnly: boolean) {
    super();
    this.block = block;
    this.readOnly = readOnly;
  }

  override eq(other: ConflictBar) {
    return (
      other.block.from === this.block.from &&
      other.block.label === this.block.label &&
      other.block.both === this.block.both &&
      other.readOnly === this.readOnly
    );
  }

  override toDOM(view: EditorView) {
    const bar = document.createElement("div");
    bar.className = "cm-conflict-bar";
    bar.setAttribute("role", "group");
    bar.setAttribute("aria-label", `Conflict with ${this.block.label}`);
    const title = document.createElement("span");
    title.className = "cm-conflict-title";
    title.textContent = `Conflict with ${this.block.label}`;
    const hint = document.createElement("span");
    hint.className = "cm-conflict-hint";
    hint.textContent = this.readOnly
      ? "A developer or admin resolves it."
      : `Yours first, then ${this.block.label}’s.`;
    bar.append(title, hint);
    if (!this.readOnly) {
      for (const { choice, label } of CHOICES) {
        if (choice === "both" && !this.block.both) continue;
        const button = document.createElement("button");
        button.type = "button";
        button.className = buttonVariants({ size: "sm" });
        button.textContent = label;
        button.addEventListener("click", () => resolveAt(view, view.posAtDOM(bar), choice));
        bar.append(button);
      }
    }
    return bar;
  }

  override ignoreEvent() {
    return true;
  }
}

/**
 * Resolves the block at `pos` as the person chose: one change in the
 * editor, which the binding sends as theirs (undoable like typing).
 */
function resolveAt(view: EditorView, pos: number, choice: Choice) {
  if (view.state.readOnly) return;
  const text = view.state.doc.toString();
  const block = findConflicts(text).find((b) => b.from <= pos && pos <= b.to);
  if (!block) return;
  const change = resolution(text, block, choice);
  view.dispatch({
    changes: change,
    selection: { anchor: change.from },
    userEvent: "input.resolve",
    scrollIntoView: true,
  });
  view.focus();
}

function conflictDecorations(state: EditorState): DecorationSet {
  const text = state.doc.toString();
  const ranges: Range<Decoration>[] = [];
  const line = (pos: number, cls: string) =>
    ranges.push(Decoration.line({ class: cls }).range(state.doc.lineAt(pos).from));
  for (const block of findConflicts(text)) {
    ranges.push(
      Decoration.widget({
        widget: new ConflictBar(block, state.readOnly),
        block: true,
        side: -1,
      }).range(block.from),
    );
    for (let pos = block.from; pos <= block.to;) {
      const current = state.doc.lineAt(pos);
      const inside = (side: { from: number; to: number }) =>
        side.from < side.to && current.from >= side.from && current.from <= side.to;
      line(
        pos,
        inside(block.yours)
          ? "cm-conflict-yours"
          : inside(block.theirs)
            ? "cm-conflict-theirs"
            : "cm-conflict-marker",
      );
      pos = current.to + 1;
    }
  }
  return Decoration.set(ranges, true);
}

/** Conflict blocks, drawn: recomputed when the text or the editability changes. */
const conflictField = StateField.define<DecorationSet>({
  create: conflictDecorations,
  update: (decorations, tr) =>
    tr.docChanged || tr.reconfigured ? conflictDecorations(tr.state) : decorations,
  provide: (field) => EditorView.decorations.from(field),
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
    conflictField,
    flashed,
    // The SQL pane is narrow next to the graph: long lines wrap.
    EditorView.lineWrapping,
    placeholder("No SQL yet. CREATE TABLE statements written here become tables in the graph."),
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

/** Brings the first conflict block after the caret (or the first) into view. */
export function nextConflict(view: EditorView) {
  const blocks = findConflicts(view.state.doc.toString());
  const head = view.state.selection.main.head;
  const block = blocks.find((b) => b.from > head) ?? blocks[0];
  if (!block) return;
  view.dispatch({
    selection: { anchor: block.yours.from },
    effects: EditorView.scrollIntoView(block.from, { y: "center" }),
  });
  view.focus();
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

/**
 * Shows SQL the graph just appended: scrolled into view (unless this person
 * is typing in the editor) and lit for a moment.
 */
export function showGraphEdit(view: EditorView, sql: string) {
  const text = view.state.doc.toString();
  const from = text.lastIndexOf(sql);
  if (from < 0) return;
  const to = from + sql.length;
  view.dispatch({
    effects: [
      flash.of({ from, to }),
      ...(view.hasFocus ? [] : [EditorView.scrollIntoView(from, { y: "nearest", yMargin: 48 })]),
    ],
  });
  const id = (flashes.get(view) ?? 0) + 1;
  flashes.set(view, id);
  window.setTimeout(() => {
    if (view.dom.isConnected && flashes.get(view) === id)
      view.dispatch({ effects: flash.of(null) });
  }, 2600);
}
