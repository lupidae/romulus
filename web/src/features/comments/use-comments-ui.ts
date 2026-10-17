import * as React from "react";

import type { Anchor } from "./model";

export type Filter = "open" | "resolved" | "mine";

/** Where the comments UI stands: the panel, the thread open in it, a new one, comment mode. */
export interface CommentsUi {
  open: boolean;
  setOpen: (open: boolean) => void;
  filter: Filter;
  setFilter: (filter: Filter) => void;
  /** The thread opened in the panel. */
  active: string | null;
  setActive: (id: string | null) => void;
  /** A new thread being written, pinned there. */
  composing: Anchor | null;
  setComposing: (anchor: Anchor | null) => void;
  /** Clicks on the graph start threads (C). */
  commentMode: boolean;
  setCommentMode: (on: boolean) => void;
  /** The panel, open on that thread. */
  openThread: (id: string) => void;
  /** The panel, writing a new thread there (comment mode ends: it did its job). */
  compose: (anchor: Anchor) => void;
  close: () => void;
}

export function useCommentsUi(initiallyOpen = false): CommentsUi {
  const [open, setOpen] = React.useState(initiallyOpen);
  const [filter, setFilter] = React.useState<Filter>("open");
  const [active, setActive] = React.useState<string | null>(null);
  const [composing, setComposing] = React.useState<Anchor | null>(null);
  const [commentMode, setCommentMode] = React.useState(false);
  return React.useMemo(
    () => ({
      open,
      setOpen,
      filter,
      setFilter,
      active,
      setActive,
      composing,
      setComposing,
      commentMode,
      setCommentMode,
      openThread: (id) => {
        setOpen(true);
        setComposing(null);
        setActive(id);
      },
      compose: (anchor) => {
        setOpen(true);
        setActive(null);
        setComposing(anchor);
        setCommentMode(false);
      },
      close: () => {
        setOpen(false);
        setComposing(null);
        setActive(null);
        setCommentMode(false);
      },
    }),
    [open, filter, active, composing, commentMode],
  );
}
