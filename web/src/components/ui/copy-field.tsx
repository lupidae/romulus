import * as React from "react";

import { CheckIcon, CopyIcon } from "@/components/icons";
import { cn } from "@/lib/utils";

import { Button } from "./button";
import { controlClasses } from "./input";
import { toast } from "./toaster";

interface CopyFieldProps {
  value: string;
  /** Names the field for screen readers: “Invitation link”. */
  label: string;
  /** Copy as soon as it appears (right after the user asked for the value). */
  autoCopy?: boolean;
  className?: string;
}

/**
 * A value to hand on (a link, a token) with a Copy button that confirms in
 * place: the button reads “Copied” for a moment, and screen readers hear it.
 * If the clipboard refuses, the text is selected so ⌘C still works.
 */
export function CopyField({ value, label, autoCopy = false, className }: CopyFieldProps) {
  const inputRef = React.useRef<HTMLInputElement>(null);
  const [copied, setCopied] = React.useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
    } catch {
      inputRef.current?.select();
      toast.error("Could not copy", { description: "Press ⌘C or Ctrl+C to copy it." });
    }
  };

  React.useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 2000);
    return () => window.clearTimeout(timer);
  }, [copied]);

  React.useEffect(() => {
    if (!autoCopy) return;
    // Quiet on failure: without a recent click the browser may refuse, and
    // the button is right there.
    navigator.clipboard.writeText(value).then(
      () => setCopied(true),
      () => undefined,
    );
  }, [autoCopy, value]);

  return (
    <div data-slot="copy-field" className={cn("flex gap-2", className)}>
      <input
        ref={inputRef}
        readOnly
        value={value}
        aria-label={label}
        spellCheck={false}
        onFocus={(e) => e.currentTarget.select()}
        className={cn(
          controlClasses,
          "h-9 min-w-0 flex-1 px-2.5 font-mono text-sm sm:h-8 sm:text-sm",
        )}
      />
      <Button
        onClick={() => void copy()}
        className="w-24 shrink-0"
        aria-label={copied ? `${label} copied` : `Copy ${label.toLowerCase()}`}
      >
        {copied ? <CheckIcon className="text-success" /> : <CopyIcon />}
        {copied ? "Copied" : "Copy"}
      </Button>
      <span role="status" aria-live="polite" className="sr-only">
        {copied ? `${label} copied to the clipboard` : ""}
      </span>
    </div>
  );
}
