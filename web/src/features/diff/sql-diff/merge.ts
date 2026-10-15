// The SQL diff's editors, loaded on demand (with CodeMirror's merge package):
// two read-only editors side by side, aligned line for line, or one editor
// with the deletions inline on a phone. Colors are the design tokens.

import {
  type Chunk,
  getChunks,
  getOriginalDoc,
  MergeView,
  uncollapseUnchanged,
  unifiedMergeView,
} from "@codemirror/merge";
import { highlightSelectionMatches, search, searchKeymap } from "@codemirror/search";
import { EditorState, type Extension, type Text } from "@codemirror/state";
import { drawSelection, EditorView, keymap, lineNumbers, ViewPlugin } from "@codemirror/view";

import {
  codeStyle,
  type Highlights,
  highlightEffect,
  type Range,
} from "@/features/schema/text/codemirror";

export type Side = "a" | "b";
export type DiffLayout = "split" | "unified";

/** Unchanged stretches keep this many lines around a change, and fold from this size. */
const COLLAPSE = { margin: 3, minSize: 4 };

export interface SqlDiffOptions {
  before: string;
  after: string;
  layout: DiffLayout;
  /** Fold long unchanged stretches (each can be unfolded with a click). */
  collapse: boolean;
  /** Accessible names of the editors: "SQL of v4". */
  labels: { a: string; b: string };
  /** The reader put the cursor at `offset` in one side's text. */
  onCursor?: (side: Side, offset: number) => void;
}

export interface SqlDiffEditor {
  /** Scrolls to an object, given its ranges on each side: its first change, else its start. */
  reveal(ranges: { a: Range[]; b: Range[] }): void;
  setHighlights(h: { a: Highlights; b: Highlights }): void;
  destroy(): void;
}

const diffTheme = EditorView.theme({
  "&.cm-merge-a .cm-changedLine, .cm-deletedChunk": {
    backgroundColor: "color-mix(in oklab, var(--color-destructive) 11%, transparent)",
  },
  "&.cm-merge-b .cm-changedLine, .cm-inlineChangedLine": {
    backgroundColor: "color-mix(in oklab, var(--color-success) 11%, transparent)",
  },
  // The selected object's statements: a bar in the margin, no tint, which
  // would read as one more change color here.
  ".cm-statement, .cm-statement-related": { backgroundColor: "transparent" },
  "&.cm-merge-a .cm-changedText, .cm-deletedChunk .cm-deletedText": {
    background: "color-mix(in oklab, var(--color-destructive) 22%, transparent)",
    borderRadius: "0.125rem",
  },
  "&.cm-merge-b .cm-changedText": {
    background: "color-mix(in oklab, var(--color-success) 24%, transparent)",
    borderRadius: "0.125rem",
  },
  "&.cm-merge-b .cm-deletedText": {
    background: "color-mix(in oklab, var(--color-destructive) 22%, transparent)",
  },
  ".cm-deletedChunk": { paddingLeft: "0" },
  // A sign next to every changed line: color is never the only signal.
  ".cm-changeGutter": { width: "1rem", paddingLeft: "0" },
  ".cm-changeGutter .cm-gutterElement": {
    fontFamily: "var(--font-mono)",
    fontWeight: "600",
    textAlign: "center",
  },
  "&.cm-merge-a .cm-changedLineGutter, .cm-deletedLineGutter": {
    background: "none",
    color: "var(--color-destructive)",
  },
  "&.cm-merge-a .cm-changedLineGutter::before, .cm-deletedLineGutter::before": { content: '"−"' },
  "&.cm-merge-b .cm-changedLineGutter": { background: "none", color: "var(--color-success)" },
  "&.cm-merge-b .cm-changedLineGutter::before": { content: '"+"' },
  ".cm-collapsedLines": {
    padding: "0.25rem 0.75rem",
    margin: "0.25rem 0",
    fontFamily: "var(--font-sans)",
    fontSize: "var(--text-xs)",
    color: "var(--color-muted-foreground)",
    background: "var(--color-muted)",
    borderBlock: "0.0625rem solid var(--color-border)",
    cursor: "pointer",
  },
  ".cm-collapsedLines:hover": { color: "var(--color-foreground)" },
  ".cm-collapsedLines::before": { content: '"↕"', marginInlineEnd: "0.5rem" },
  ".cm-collapsedLines::after": { content: "none" },
});

function sideExtensions(label: string, side: Side, options: SqlDiffOptions): Extension {
  return [
    lineNumbers(),
    drawSelection(),
    highlightSelectionMatches(),
    search({ top: true }),
    keymap.of(searchKeymap),
    // Before the shared style: earlier extensions win, and this one overrides it.
    diffTheme,
    codeStyle("sql"),
    EditorState.readOnly.of(true),
    EditorView.editable.of(false),
    EditorState.phrases.of({ "$ unchanged lines": "$ unchanged lines: show them" }),
    // Read-only still takes focus and a caret: people select, search, copy,
    // and the caret picks the object it is in.
    EditorView.contentAttributes.of({
      "aria-label": label,
      "aria-readonly": "true",
      tabindex: "0",
    }),
    EditorView.updateListener.of((update) => {
      if (!options.onCursor || !update.selectionSet) return;
      if (!update.transactions.some((tr) => tr.isUserEvent("select"))) return;
      options.onCursor(side, update.state.selection.main.head);
    }),
  ];
}

/**
 * The stretches the merge view folds on one side, gaps between chunks in
 * order (as `@codemirror/merge` computes them), so the gap holding a position
 * can be unfolded on both sides: unchanged gaps are the same lines on each.
 */
function gaps(doc: Text, chunks: readonly Chunk[], side: Side): (Range | null)[] {
  const out: (Range | null)[] = [];
  let prevLine = 1;
  for (let i = 0; ; i++) {
    const chunk = i < chunks.length ? chunks[i]! : null;
    const from = i ? prevLine + COLLAPSE.margin : 1;
    const to = chunk
      ? doc.lineAt(side === "a" ? chunk.fromA : chunk.fromB).number - 1 - COLLAPSE.margin
      : doc.lines;
    out.push(
      to - from + 1 >= COLLAPSE.minSize ? { from: doc.line(from).from, to: doc.line(to).to } : null,
    );
    if (!chunk) break;
    prevLine = doc.lineAt(Math.min(doc.length, side === "a" ? chunk.toA : chunk.toB)).number;
  }
  return out;
}

function intersects(chunk: Chunk, side: Side, r: Range): boolean {
  const from = side === "a" ? chunk.fromA : chunk.fromB;
  const to = side === "a" ? chunk.toA : chunk.toB;
  // A chunk with nothing on this side sits between two of its lines.
  return from === to ? from >= r.from && from <= r.to : from < r.to && to > r.from;
}

function firstChunk(chunks: readonly Chunk[], ranges: { a: Range[]; b: Range[] }) {
  return chunks.find(
    (c) =>
      ranges.b.some((r) => intersects(c, "b", r)) || ranges.a.some((r) => intersects(c, "a", r)),
  );
}

function scrollTo(view: EditorView, pos: number) {
  const at = Math.min(pos, view.state.doc.length);
  view.dispatch({ effects: EditorView.scrollIntoView(at, { y: "start", yMargin: 48 }) });
}

export function createSqlDiff(parent: HTMLElement, options: SqlDiffOptions): SqlDiffEditor {
  const collapse = options.collapse ? COLLAPSE : undefined;
  if (options.layout === "split") {
    const merge = new MergeView({
      parent,
      a: { doc: options.before, extensions: sideExtensions(options.labels.a, "a", options) },
      b: { doc: options.after, extensions: sideExtensions(options.labels.b, "b", options) },
      gutter: true,
      highlightChanges: true,
      collapseUnchanged: collapse,
    });
    merge.dom.style.height = "100%";
    // The gap holding `pos` on `side`, unfolded on both sides.
    const unfold = (side: Side, pos: number) => {
      if (!collapse) return;
      const a = gaps(merge.a.state.doc, merge.chunks, "a");
      const b = gaps(merge.b.state.doc, merge.chunks, "b");
      const k = (side === "a" ? a : b).findIndex((g) => g && pos >= g.from && pos <= g.to);
      if (k < 0) return;
      if (a[k]) merge.a.dispatch({ effects: uncollapseUnchanged.of(a[k].from) });
      if (b[k]) merge.b.dispatch({ effects: uncollapseUnchanged.of(b[k].from) });
    };
    return {
      reveal(ranges) {
        const chunk = firstChunk(merge.chunks, ranges);
        // Both editors scroll together (one scroller): moving one moves both.
        if (chunk && ranges.b.length) scrollTo(merge.b, chunk.fromB);
        else if (chunk) scrollTo(merge.a, chunk.fromA);
        else {
          const side: Side = ranges.b.length ? "b" : "a";
          const at = (ranges[side][0] ?? { from: 0 }).from;
          unfold(side, at);
          scrollTo(side === "a" ? merge.a : merge.b, at);
        }
      },
      setHighlights(h) {
        merge.a.dispatch({ effects: highlightEffect.of(h.a) });
        merge.b.dispatch({ effects: highlightEffect.of(h.b) });
      },
      destroy() {
        merge.destroy();
      },
    };
  }

  // The old side has no editor here: its lines are drawn inside the
  // deleted chunks. They take clicks (the cursor of side a) and highlights.
  let oldHighlights: Highlights = { related: [] };
  // The editor then puts its own cursor where the chunk sits in the new
  // text: that one is not the reader's pick.
  let oldPickAt = -Infinity;
  const unified: SqlDiffOptions = {
    ...options,
    onCursor: (side, offset) => {
      if (performance.now() - oldPickAt < 400) return;
      options.onCursor?.(side, offset);
    },
  };
  /** The deleted chunk's lines, each with its range in the old text. */
  const deletedLines = (view: EditorView, chunkDom: Element) => {
    const at = view.posAtDOM(chunkDom);
    const chunk = (getChunks(view.state)?.chunks ?? []).find((c) => c.fromB === at);
    if (!chunk) return [];
    const original = getOriginalDoc(view.state);
    const first = original.lineAt(chunk.fromA).number;
    return [...chunkDom.querySelectorAll<HTMLElement>(".cm-deletedLine")].map((dom, i) => {
      const line = original.line(Math.min(first + i, original.lines));
      return { dom, from: line.from, to: line.to };
    });
  };
  const paintOld = (view: EditorView) => {
    const ranges = [
      ...(oldHighlights.primary ? [{ ...oldHighlights.primary, cls: "cm-statement" }] : []),
      ...oldHighlights.related.map((r) => ({ ...r, cls: "cm-statement-related" })),
    ];
    for (const chunkDom of view.contentDOM.querySelectorAll(".cm-deletedChunk")) {
      for (const line of deletedLines(view, chunkDom)) {
        const hit = ranges.find((r) => line.from < r.to && line.to >= r.from);
        line.dom.classList.toggle("cm-statement", hit?.cls === "cm-statement");
        line.dom.classList.toggle("cm-statement-related", hit?.cls === "cm-statement-related");
      }
    }
  };
  const oldSide = [
    ViewPlugin.define((view) => {
      view.requestMeasure({ read: () => null, write: () => paintOld(view) });
      return {
        // Chunks are drawn during the update: paint after it.
        update: (update) =>
          update.view.requestMeasure({ read: () => null, write: () => paintOld(update.view) }),
      };
    }),
  ];

  const view = new EditorView({
    parent,
    state: EditorState.create({
      doc: options.after,
      extensions: [
        sideExtensions(options.labels.b, "b", unified),
        unifiedMergeView({
          original: options.before,
          gutter: true,
          highlightChanges: true,
          mergeControls: false,
          syntaxHighlightDeletions: true,
          collapseUnchanged: collapse,
        }),
        oldSide,
        EditorView.theme({ "&": { height: "100%" } }),
        // One column in a narrow pane: long lines wrap instead of hiding.
        EditorView.lineWrapping,
      ],
    }),
  });
  // A click in a deleted line: the cursor of side a. A native listener, as
  // the editor leaves events inside widgets (the deleted chunks) alone.
  const onOldSide = (event: MouseEvent) => {
    const target = event.target instanceof Element ? event.target : null;
    const lineDom = target?.closest(".cm-deletedLine");
    const chunkDom = lineDom?.closest(".cm-deletedChunk");
    if (!lineDom || !chunkDom) return;
    const line = deletedLines(view, chunkDom).find((l) => l.dom === lineDom);
    if (!line) return;
    oldPickAt = performance.now();
    options.onCursor?.("a", line.from);
  };
  view.contentDOM.addEventListener("mousedown", onOldSide);
  const chunks = () => getChunks(view.state)?.chunks ?? [];
  return {
    reveal(ranges) {
      // Deletions are drawn where they were: at the chunk's place in the new text.
      const chunk = firstChunk(chunks(), ranges);
      const at = chunk ? chunk.fromB : (ranges.b[0]?.from ?? 0);
      if (!chunk && collapse) {
        const k = gaps(view.state.doc, chunks(), "b").find((g) => g && at >= g.from && at <= g.to);
        if (k) view.dispatch({ effects: uncollapseUnchanged.of(k.from) });
      }
      scrollTo(view, at);
    },
    setHighlights(h) {
      oldHighlights = h.a;
      view.dispatch({ effects: highlightEffect.of(h.b) });
      paintOld(view);
    },
    destroy() {
      view.contentDOM.removeEventListener("mousedown", onOldSide);
      view.destroy();
    },
  };
}
