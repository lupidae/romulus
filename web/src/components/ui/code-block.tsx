import * as React from "react";

import { CheckIcon, CopyIcon } from "@/components/icons";
import { cn } from "@/lib/utils";

import { Button } from "./button";
import { toast } from "./toaster";

interface CodeBlockProps {
  /** The text, copied exactly as shown. */
  code: string;
  /** Names it for screen readers and the copy button: “Push command”. */
  label: string;
  className?: string;
}

/**
 * A few lines to paste somewhere else (a command, a CI step), with a Copy
 * button that confirms in place. Long lines scroll sideways rather than
 * wrap, so what is copied is what is seen.
 */
export function CodeBlock({ code, label, className }: CodeBlockProps) {
  const [copied, setCopied] = React.useState(false);
  const preRef = React.useRef<HTMLPreElement>(null);

  React.useEffect(() => {
    if (!copied) return;
    const timer = window.setTimeout(() => setCopied(false), 2000);
    return () => window.clearTimeout(timer);
  }, [copied]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
    } catch {
      const selection = window.getSelection();
      if (preRef.current && selection) selection.selectAllChildren(preRef.current);
      toast.error("Could not copy", { description: "Press ⌘C or Ctrl+C to copy it." });
    }
  };

  return (
    <div
      data-slot="code-block"
      className={cn("relative rounded-lg border border-border bg-muted", className)}
    >
      <pre
        ref={preRef}
        aria-label={label}
        className="overflow-x-auto rounded-lg py-3 pr-24 pl-3 font-mono text-sm leading-5 text-foreground"
      >
        <code>{code}</code>
      </pre>
      <Button
        size="sm"
        onClick={() => void copy()}
        className="absolute top-2 right-2"
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
