import type { Extension } from "@codemirror/state";
import { useEffect, useRef, useState } from "react";

import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

import type { CodeLanguage, Editor, Highlights, Range } from "./codemirror";

export interface CodeViewProps {
  /**
   * The text. With `extensions` (a Yjs binding owns the text) it only seeds
   * the editor: later values are ignored, never pushed in as a replace.
   */
  doc: string;
  language: CodeLanguage;
  /** Accessible name of the text area. */
  label: string;
  readOnly: boolean;
  /** Read once, when the editor is created (memoize them; a new array is ignored, with a warning in dev). */
  extensions?: Extension[];
  /**
   * Extensions that draw over the text without owning it (comment threads):
   * read once like `extensions`, but `doc` still sets the text.
   */
  overlay?: Extension[];
  highlights?: Highlights;
  /** Scrolled to whenever a new object is passed (and the view is visible). */
  reveal?: Range | null;
  /** Off-screen tabs keep their editor but skip work until shown. */
  active: boolean;
  onCursor?: (offset: number) => void;
  /** The editor, once loaded: lets the pane open its search panel. */
  onEditor?: (editor: Editor | null) => void;
  className?: string;
}

const NO_HIGHLIGHTS: Highlights = { related: [] };

/**
 * One CodeMirror editor, created in an effect (StrictMode mounts twice; a view
 * created during render would leak) from a lazily loaded chunk. Shows the
 * shape of code while that chunk loads.
 */
export function CodeView({
  doc,
  language,
  label,
  readOnly,
  extensions,
  overlay,
  highlights = NO_HIGHLIGHTS,
  reveal,
  active,
  onCursor,
  onEditor,
  className,
}: CodeViewProps) {
  const host = useRef<HTMLDivElement>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  const latest = useRef({ doc, readOnly, onCursor, onEditor });
  useEffect(() => {
    latest.current = { doc, readOnly, onCursor, onEditor };
  });

  // Extensions are the editor's for its life: a binding torn down and rebuilt
  // on every render would drop the collaboration. The first value is kept.
  const [fixedExtensions] = useState(extensions);
  const [fixedOverlay] = useState(overlay);
  const warned = useRef(false);
  useEffect(() => {
    if (import.meta.env.DEV && extensions !== fixedExtensions && !warned.current) {
      warned.current = true;
      console.warn(
        "CodeView: `extensions` changed after the editor was created and was ignored. " +
          "Memoize them, or remount the editor (a new `key`) to apply new ones.",
      );
    }
  }, [extensions, fixedExtensions]);
  // A binding owns the text: the `doc` prop must not overwrite it.
  const owned = (fixedExtensions?.length ?? 0) > 0;

  // Created the first time the tab is shown, then kept.
  const [wanted, setWanted] = useState(active);
  if (active && !wanted) setWanted(true);

  useEffect(() => {
    if (!wanted || !host.current) return;
    let created: Editor | null = null;
    let cancelled = false;
    void import("./codemirror").then(({ createEditor }) => {
      if (cancelled || !host.current) return;
      created = createEditor(host.current, {
        doc: latest.current.doc,
        language,
        readOnly: latest.current.readOnly,
        label,
        extensions: [...(fixedExtensions ?? []), ...(fixedOverlay ?? [])],
        onCursor: (offset) => latest.current.onCursor?.(offset),
      });
      setEditor(created);
      latest.current.onEditor?.(created);
    });
    return () => {
      cancelled = true;
      created?.destroy();
      setEditor(null);
      latest.current.onEditor?.(null);
    };
  }, [wanted, language, label, fixedExtensions, fixedOverlay]);

  useEffect(() => {
    if (!owned) editor?.setDoc(doc);
  }, [editor, doc, owned]);

  useEffect(() => {
    editor?.setReadOnly(readOnly);
  }, [editor, readOnly]);

  useEffect(() => {
    editor?.setHighlights(highlights);
  }, [editor, highlights]);

  useEffect(() => {
    if (editor && active && reveal) editor.reveal(reveal);
  }, [editor, active, reveal]);

  useEffect(() => {
    if (active) latest.current.onEditor?.(editor);
  }, [editor, active]);

  return (
    <div className={cn("relative h-full min-h-0", className)}>
      <div ref={host} className="h-full" />
      {!editor && (
        <div aria-hidden className="absolute inset-0 grid content-start gap-2.5 bg-card px-4 py-3">
          {[64, 88, 72, 40, 80, 56, 92, 48, 70, 36].map((w, i) => (
            <Skeleton key={i} className="h-3" style={{ width: `${w}%` }} />
          ))}
        </div>
      )}
    </div>
  );
}
