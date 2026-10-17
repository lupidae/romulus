import * as React from "react";

import { Button } from "@/components/ui/button";
import { MentionTextarea, type MentionPerson } from "@/components/ui/mention-textarea";
import { cn } from "@/lib/utils";

import { MAX_BODY, mentionsIn } from "../model";

interface ComposerProps {
  /** The text area's accessible name: "Reply", "Comment on users.email". */
  label: string;
  placeholder: string;
  submitLabel: string;
  people: readonly MentionPerson[];
  /** Sends; the composer empties once it resolves (and keeps the text if it throws). */
  onSubmit: (body: string, mentions: string[]) => Promise<unknown> | void;
  onCancel?: () => void;
  /** Writing (true) or not any more (false): who is typing, for the others. */
  onTyping?: (typing: boolean) => void;
  initial?: string;
  /** Takes the focus when it appears (a new comment asked for, an edit). */
  focusOnMount?: boolean;
  className?: string;
}

/** Idle this long, the others stop seeing "is typing". */
const TYPING_FOR = 4000;

/**
 * Where a comment is written: `@` suggests members, Enter sends, Shift+Enter
 * starts a new line, Esc cancels (when there is something to cancel).
 */
export function Composer({
  label,
  placeholder,
  submitLabel,
  people,
  onSubmit,
  onCancel,
  onTyping,
  initial = "",
  focusOnMount = false,
  className,
}: ComposerProps) {
  const [body, setBody] = React.useState(initial);
  const [sending, setSending] = React.useState(false);
  const hint = React.useId();
  const idle = React.useRef(0);
  const typingRef = React.useRef(onTyping);
  React.useEffect(() => {
    typingRef.current = onTyping;
  });
  const stopTyping = React.useCallback(() => {
    window.clearTimeout(idle.current);
    typingRef.current?.(false);
  }, []);
  React.useEffect(() => stopTyping, [stopTyping]);

  const length = [...body].length;
  const empty = body.trim() === "";
  const tooLong = length > MAX_BODY;

  const send = async () => {
    if (empty || tooLong || sending) return;
    setSending(true);
    stopTyping();
    try {
      await onSubmit(body.trim(), mentionsIn(body, people));
      setBody("");
    } catch {
      // The caller said why; the text stays to try again.
    } finally {
      setSending(false);
    }
  };

  return (
    <div className={cn("grid gap-2", className)}>
      <MentionTextarea
        aria-label={label}
        aria-describedby={hint}
        aria-invalid={tooLong || undefined}
        placeholder={placeholder}
        value={body}
        people={people}
        focusOnMount={focusOnMount}
        className="min-h-16 resize-none text-base sm:text-sm"
        onValueChange={(next) => {
          setBody(next);
          window.clearTimeout(idle.current);
          if (next.trim() === "") {
            onTyping?.(false);
            return;
          }
          onTyping?.(true);
          idle.current = window.setTimeout(() => typingRef.current?.(false), TYPING_FOR);
        }}
        onBlur={stopTyping}
        onSubmit={() => void send()}
        onKeyDown={(event) => {
          if (event.key === "Escape" && onCancel) {
            event.preventDefault();
            event.stopPropagation();
            stopTyping();
            onCancel();
          }
        }}
      />
      <div className="flex items-center gap-2">
        <p id={hint} className="mr-auto min-w-0 text-xs text-faint-foreground">
          {tooLong ? (
            <span className="text-destructive">
              {length - MAX_BODY} characters over the {MAX_BODY} a comment holds
            </span>
          ) : (
            <>
              @ to mention
              <span className="sr-only">, Enter to send, Shift+Enter for a new line</span>
            </>
          )}
        </p>
        {onCancel && (
          <Button
            variant="ghost"
            size="sm"
            onClick={() => {
              stopTyping();
              onCancel();
            }}
          >
            Cancel
          </Button>
        )}
        <Button
          variant="primary"
          size="sm"
          aria-keyshortcuts="Enter"
          disabled={empty || tooLong}
          loading={sending}
          onClick={() => void send()}
        >
          {submitLabel}
          <span aria-hidden className="text-xs opacity-70">
            ↵
          </span>
        </Button>
      </div>
    </div>
  );
}
