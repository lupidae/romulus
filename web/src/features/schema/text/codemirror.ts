// CodeMirror for the text pane, loaded on demand (the editor and its
// languages are a separate chunk). Colors are the design tokens, so the
// editor follows the theme without a second palette.

import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import { json } from "@codemirror/lang-json";
import { PostgreSQL, sql } from "@codemirror/lang-sql";
import {
  bracketMatching,
  HighlightStyle,
  type StreamParser,
  StreamLanguage,
  syntaxHighlighting,
} from "@codemirror/language";
import {
  highlightSelectionMatches,
  openSearchPanel,
  search,
  searchKeymap,
} from "@codemirror/search";
import {
  Compartment,
  EditorState,
  type Extension,
  RangeSetBuilder,
  StateEffect,
  StateField,
} from "@codemirror/state";
import {
  Decoration,
  type DecorationSet,
  drawSelection,
  EditorView,
  highlightActiveLine,
  highlightActiveLineGutter,
  keymap,
  lineNumbers,
} from "@codemirror/view";
import { tags as t } from "@lezer/highlight";

export type CodeLanguage = "sql" | "json" | "dbml" | "mermaid";

export interface Range {
  from: number;
  to: number;
}

export interface Highlights {
  /** The selected table's defining statement. */
  primary?: Range;
  /** Its other statements (foreign keys, indexes, comments). */
  related: Range[];
}

// ---- Theme ------------------------------------------------------------------

const theme = EditorView.theme({
  "&": {
    height: "100%",
    color: "var(--color-foreground)",
    backgroundColor: "var(--color-card)",
    fontSize: "var(--text-sm)",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": {
    fontFamily: "var(--font-mono)",
    lineHeight: "1.25rem",
    overscrollBehavior: "contain",
  },
  ".cm-content": { padding: "0.5rem 0", caretColor: "var(--color-primary)" },
  ".cm-line": { padding: "0 1rem 0 0.75rem" },
  ".cm-gutters": {
    backgroundColor: "var(--color-card)",
    color: "var(--color-faint-foreground)",
    border: "none",
  },
  ".cm-lineNumbers .cm-gutterElement": { padding: "0 0.25rem 0 0.75rem", minWidth: "2.5rem" },
  ".cm-activeLine": {
    backgroundColor: "color-mix(in oklab, var(--color-accent) 55%, transparent)",
  },
  ".cm-activeLineGutter": { backgroundColor: "transparent", color: "var(--color-foreground)" },
  ".cm-cursor, .cm-dropCursor": {
    borderLeftColor: "var(--color-primary)",
    borderLeftWidth: "0.125rem",
  },
  "&.cm-focused > .cm-scroller > .cm-selectionLayer .cm-selectionBackground, .cm-selectionBackground, ::selection":
    { backgroundColor: "var(--color-selection)" },
  ".cm-matchingBracket": {
    backgroundColor: "color-mix(in oklab, var(--color-primary) 14%, transparent)",
    outline: "none",
  },
  ".cm-selectionMatch": {
    backgroundColor: "color-mix(in oklab, var(--color-info) 12%, transparent)",
  },
  ".cm-searchMatch": {
    backgroundColor: "color-mix(in oklab, var(--color-warning) 22%, transparent)",
    outline: "0.0625rem solid color-mix(in oklab, var(--color-warning) 45%, transparent)",
    borderRadius: "0.125rem",
  },
  ".cm-searchMatch.cm-searchMatch-selected": {
    backgroundColor: "color-mix(in oklab, var(--color-warning) 45%, transparent)",
  },
  // The statement of the table selected in the graph.
  ".cm-statement": {
    backgroundColor: "color-mix(in oklab, var(--color-primary) 7%, transparent)",
    boxShadow: "inset 0.125rem 0 0 var(--color-primary)",
  },
  ".cm-statement-related": {
    backgroundColor: "color-mix(in oklab, var(--color-primary) 3.5%, transparent)",
    boxShadow: "inset 0.125rem 0 0 color-mix(in oklab, var(--color-primary) 45%, transparent)",
  },
  // The search panel, in the kit's terms.
  ".cm-panels": {
    backgroundColor: "var(--color-muted)",
    color: "var(--color-foreground)",
    fontFamily: "var(--font-sans)",
    fontSize: "var(--text-sm)",
  },
  ".cm-panels.cm-panels-top": { borderBottom: "0.0625rem solid var(--color-border)" },
  ".cm-panels.cm-panels-bottom": { borderTop: "0.0625rem solid var(--color-border)" },
  ".cm-search": {
    padding: "0.5rem 2.25rem 0.5rem 0.75rem",
    display: "flex",
    flexWrap: "wrap",
    gap: "0.375rem",
    alignItems: "center",
  },
  ".cm-search br": { display: "none" },
  ".cm-search label": {
    display: "inline-flex",
    alignItems: "center",
    gap: "0.25rem",
    color: "var(--color-muted-foreground)",
  },
  ".cm-textfield": {
    height: "1.75rem",
    padding: "0 0.5rem",
    border: "0.0625rem solid var(--color-input)",
    borderRadius: "var(--radius-md)",
    backgroundColor: "var(--color-card)",
    color: "var(--color-foreground)",
    fontSize: "var(--text-sm)",
    outline: "none",
    margin: "0",
  },
  ".cm-textfield:focus-visible": {
    borderColor: "var(--color-ring)",
    boxShadow: "0 0 0 0.1875rem color-mix(in oklab, var(--color-ring) 20%, transparent)",
  },
  ".cm-button": {
    height: "1.75rem",
    padding: "0 0.625rem",
    border: "0.0625rem solid var(--color-border)",
    borderRadius: "var(--radius-md)",
    backgroundImage: "none",
    backgroundColor: "var(--color-card)",
    color: "var(--color-foreground)",
    fontSize: "var(--text-sm)",
    margin: "0",
    cursor: "pointer",
  },
  ".cm-button:hover": { backgroundColor: "var(--color-secondary)" },
  ".cm-button:focus-visible": {
    outline: "0.125rem solid var(--color-ring)",
    outlineOffset: "0.125rem",
  },
  ".cm-panel.cm-search [name=close]": {
    position: "absolute",
    top: "0.5rem",
    right: "0.5rem",
    width: "1.75rem",
    height: "1.75rem",
    borderRadius: "var(--radius-md)",
    color: "var(--color-muted-foreground)",
    fontSize: "1rem",
    cursor: "pointer",
  },
  ".cm-panel.cm-search [name=close]:hover": { backgroundColor: "var(--color-accent)" },
  'input[type="checkbox"]': { accentColor: "var(--color-primary)", margin: "0" },
});

const highlight = HighlightStyle.define([
  {
    tag: [t.keyword, t.operatorKeyword, t.modifier, t.controlKeyword, t.definitionKeyword],
    color: "var(--color-syntax-keyword)",
  },
  { tag: [t.typeName, t.className, t.namespace], color: "var(--color-syntax-type)" },
  { tag: [t.string, t.special(t.string), t.regexp], color: "var(--color-syntax-string)" },
  { tag: [t.number, t.bool, t.null, t.atom], color: "var(--color-syntax-number)" },
  {
    tag: [t.function(t.variableName), t.function(t.propertyName), t.standard(t.name)],
    color: "var(--color-syntax-function)",
  },
  { tag: [t.propertyName, t.attributeName], color: "var(--color-syntax-property)" },
  {
    tag: [t.comment, t.lineComment, t.blockComment],
    color: "var(--color-syntax-comment)",
    fontStyle: "italic",
  },
  {
    tag: [t.punctuation, t.bracket, t.separator, t.operator],
    color: "var(--color-syntax-punctuation)",
  },
  { tag: [t.special(t.name), t.quote], color: "var(--color-foreground)" },
  { tag: t.meta, color: "var(--color-syntax-comment)" },
]);

// ---- DBML and Mermaid: small stream modes, enough to read them -------------

const DBML_KEYWORDS =
  /^(table|ref|enum|note|project|tablegroup|indexes|as|pk|increment|unique|default|null|not|primary|key|delete|update|cascade|restrict|set|no|action|headercolor|database_type)\b/i;

const dbml: StreamParser<{ inNote: boolean }> = {
  name: "dbml",
  startState: () => ({ inNote: false }),
  token(stream, state) {
    if (state.inNote) {
      if (stream.match(/.*?'''/)) state.inNote = false;
      else stream.skipToEnd();
      return "string";
    }
    if (stream.eatSpace()) return null;
    if (stream.match("//")) {
      stream.skipToEnd();
      return "comment";
    }
    if (stream.match("'''")) {
      state.inNote = true;
      return "string";
    }
    if (stream.match(/^'(?:[^'\\]|\\.)*'/) || stream.match(/^`[^`]*`/)) return "string";
    if (stream.match(/^"(?:[^"\\]|\\.)*"/)) return "variableName";
    if (stream.match(/^-?\d+(\.\d+)?/)) return "number";
    if (stream.match(/^(<>|[<>-])/)) return "operator";
    if (stream.match(/^[{}[\]()]/)) return "bracket";
    if (stream.match(/^[:,.]/)) return "punctuation";
    if (stream.match(DBML_KEYWORDS)) return "keyword";
    if (stream.match(/^[\w$]+/)) {
      // The word after a column name is its type.
      return /^\s+[\w"]/.test(stream.string.slice(stream.pos)) &&
        stream.string.slice(0, stream.start).trim() === ""
        ? "propertyName"
        : "typeName";
    }
    stream.next();
    return null;
  },
};

const mermaid: StreamParser<unknown> = {
  name: "mermaid",
  token(stream) {
    if (stream.eatSpace()) return null;
    if (stream.match("%%")) {
      stream.skipToEnd();
      return "comment";
    }
    if (stream.match(/^"[^"]*"/)) return "string";
    if (stream.match(/^(erDiagram|PK|FK|UK)\b/)) return "keyword";
    if (stream.match(/^[|}o][|o{]?(--|\.\.)[|o{][|}o]?/)) return "operator";
    if (stream.match(/^[{}:]/)) return "bracket";
    if (stream.match(/^[\w()[\]]+/)) {
      // Attribute lines read `type name`: the first word is the type.
      return stream.string.slice(0, stream.start).trim() === "" &&
        /^\s+\w/.test(stream.string.slice(stream.pos))
        ? "typeName"
        : "variableName";
    }
    stream.next();
    return null;
  },
};

function language(lang: CodeLanguage): Extension {
  switch (lang) {
    case "sql":
      return sql({ dialect: PostgreSQL });
    case "json":
      return json();
    case "dbml":
      return StreamLanguage.define(dbml);
    case "mermaid":
      return StreamLanguage.define(mermaid);
  }
}

// ---- Statement highlights ----------------------------------------------------

const setHighlights = StateEffect.define<Highlights>();

function lineDecorations(state: EditorState, h: Highlights): DecorationSet {
  const lines = new Map<number, string>();
  const mark = (r: Range, cls: string) => {
    const first = state.doc.lineAt(Math.min(r.from, state.doc.length)).number;
    const last = state.doc.lineAt(Math.min(Math.max(r.from, r.to - 1), state.doc.length)).number;
    for (let n = first; n <= last; n++)
      if (!lines.has(n) || cls === "cm-statement") lines.set(n, cls);
  };
  for (const r of h.related) mark(r, "cm-statement-related");
  if (h.primary) mark(h.primary, "cm-statement");
  const builder = new RangeSetBuilder<Decoration>();
  for (const n of [...lines.keys()].sort((a, b) => a - b)) {
    builder.add(
      state.doc.line(n).from,
      state.doc.line(n).from,
      Decoration.line({ class: lines.get(n)! }),
    );
  }
  return builder.finish();
}

const highlightField = StateField.define<{ h: Highlights; deco: DecorationSet }>({
  create: () => ({ h: { related: [] }, deco: Decoration.none }),
  update(value, tr) {
    for (const e of tr.effects)
      if (e.is(setHighlights)) return { h: e.value, deco: lineDecorations(tr.state, e.value) };
    return tr.docChanged ? { h: value.h, deco: value.deco.map(tr.changes) } : value;
  },
  provide: (f) => EditorView.decorations.from(f, (v) => v.deco),
});

// ---- The editor ------------------------------------------------------------------

export interface EditorOptions {
  doc: string;
  language: CodeLanguage;
  readOnly: boolean;
  /** Accessible name of the text area. */
  label: string;
  /** More extensions: a Yjs binding for a draft (`yCollab`), linting… */
  extensions?: Extension[];
  /** The cursor moved by the user (pointer or keys) to this offset. */
  onCursor?: (offset: number) => void;
}

export interface Editor {
  view: EditorView;
  setDoc(doc: string): void;
  setHighlights(h: Highlights): void;
  /** Without recreating the editor (and its binding): a role change, say. */
  setReadOnly(readOnly: boolean): void;
  /** Scrolls a range near the top of the pane, without moving the cursor. */
  reveal(range: Range): void;
  openSearch(): void;
  destroy(): void;
}

function editability(readOnly: boolean): Extension {
  return [
    EditorState.readOnly.of(readOnly),
    EditorView.editable.of(!readOnly),
    EditorView.contentAttributes.of(readOnly ? { "aria-readonly": "true" } : {}),
  ];
}

export function createEditor(parent: HTMLElement, options: EditorOptions): Editor {
  const editable = new Compartment();
  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc: options.doc,
      extensions: [
        lineNumbers(),
        highlightActiveLineGutter(),
        highlightActiveLine(),
        drawSelection(),
        bracketMatching(),
        highlightSelectionMatches(),
        search({ top: true }),
        // A collaborative binding brings its own undo (Y.UndoManager).
        ...(options.readOnly || options.extensions?.length
          ? []
          : [history(), keymap.of(historyKeymap)]),
        keymap.of([...searchKeymap, ...defaultKeymap]),
        language(options.language),
        syntaxHighlighting(highlight),
        theme,
        highlightField,
        editable.of(editability(options.readOnly)),
        // Read-only still takes focus and a caret: people select, search and
        // copy, and the caret is what selects a table in the graph.
        EditorView.contentAttributes.of({ "aria-label": options.label, tabindex: "0" }),
        EditorView.updateListener.of((update) => {
          if (!options.onCursor || !update.selectionSet) return;
          if (
            !update.transactions.some((tr) => tr.isUserEvent("select") || tr.isUserEvent("input"))
          )
            return;
          options.onCursor(update.state.selection.main.head);
        }),
        ...(options.extensions ?? []),
      ],
    }),
  });

  return {
    view,
    setDoc(doc) {
      if (doc === view.state.doc.toString()) return;
      view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: doc } });
    },
    setHighlights(h) {
      view.dispatch({ effects: setHighlights.of(h) });
    },
    setReadOnly(readOnly) {
      if (view.state.readOnly === readOnly) return;
      view.dispatch({ effects: editable.reconfigure(editability(readOnly)) });
    },
    reveal(range) {
      const at = Math.min(range.from, view.state.doc.length);
      view.dispatch({ effects: EditorView.scrollIntoView(at, { y: "start", yMargin: 48 }) });
    },
    openSearch() {
      openSearchPanel(view);
    },
    destroy() {
      view.destroy();
    },
  };
}
