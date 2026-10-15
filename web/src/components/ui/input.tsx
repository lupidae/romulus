import type * as React from "react";

import { cn } from "@/lib/utils";

// Shared by Input, Textarea and the Select trigger so every control lines up.
// 16px text below `sm`: iOS zooms into any focused field smaller than that.
export const controlClasses = cn(
  "w-full min-w-0 rounded-md border border-input bg-card text-foreground shadow-control",
  "text-lg sm:text-base placeholder:text-faint-foreground",
  "transition-[border-color,box-shadow] outline-none",
  "hover:border-border-strong",
  "focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/20",
  "aria-invalid:border-destructive aria-invalid:focus-visible:ring-destructive/20",
  "disabled:cursor-not-allowed disabled:bg-muted disabled:text-faint-foreground",
);

export function Input({ className, type = "text", ...props }: React.ComponentProps<"input">) {
  return (
    <input
      data-slot="input"
      type={type}
      className={cn(
        controlClasses,
        "h-9 px-2.5 sm:h-8",
        "file:mr-2 file:border-0 file:bg-transparent file:text-sm file:font-medium",
        className,
      )}
      {...props}
    />
  );
}

export function Textarea({ className, ...props }: React.ComponentProps<"textarea">) {
  return (
    <textarea
      data-slot="textarea"
      className={cn(controlClasses, "field-sizing-content min-h-20 px-2.5 py-1.5", className)}
      {...props}
    />
  );
}
