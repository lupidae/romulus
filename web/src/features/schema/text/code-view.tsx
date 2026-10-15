import type { Extension } from "@codemirror/state";
import { useEffect, useRef, useState } from "react";

import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";

import type { CodeLanguage, Editor, Highlights, Range } from "./codemirror";

export interface CodeViewProps {
  doc: string;
  language: CodeLanguage;
  /** Accessible name of the text area. */
  label: string;
  readOnly: boolean;
  /** Fixed for the editor's life: changing them creates a new editor. */
  extensions?: Extension[];
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
  highlights = NO_HIGHLIGHTS,
  reveal,
  active,
  onCursor,
  onEditor,
  className,
}: CodeViewProps) {
  const host = useRef<HTMLDivElement>(null);
  const [editor, setEditor] = useState<Editor | null>(null);
  const latest = useRef({ doc, onCursor, onEditor });
  useEffect(() => {
    latest.current = { doc, onCursor, onEditor };
  });

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
        readOnly,
        label,
        extensions,
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
  }, [wanted, language, readOnly, label, extensions]);

  useEffect(() => {
    editor?.setDoc(doc);
  }, [editor, doc]);

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
