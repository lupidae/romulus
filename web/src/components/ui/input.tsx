import type * as React from "react";

import { cn } from "@/lib/utils";

// Shared by Input, Textarea and the Select trigger so every control lines up.
// 16px text below `sm`: iOS zooms into any focused field smaller than that.
export const controlClasses = cn(
  "w-full min-w-0 rounded-md border border-input bg-card text-foreground shadow-control",
  "text-lg placeholder:text-faint-foreground sm:text-base",
  "transition-[border-color,box-shadow] outline-none",
  "hover:border-border-strong",
  "focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/20",
  "aria-invalid:border-destructive aria-invalid:focus-visible:ring-destructive/20",
  "disabled:cursor-not-allowed disabled:bg-muted disabled:text-faint-foreground",
);

interface InputProps extends Omit<React.ComponentProps<"input">, "prefix"> {
  /**
   * Fixed text before the value, inside the control: `romulus.app/app/` before
   * a slug. Not part of the value; id and aria-* still go to the input.
   */
  prefix?: React.ReactNode;
}

export function Input({ className, type = "text", prefix, ...props }: InputProps) {
  if (prefix !== undefined) {
    return (
      <div
        data-slot="input-group"
        className={cn(
          controlClasses,
          "flex h-9 items-center overflow-hidden sm:h-8",
          "has-[input:focus-visible]:border-ring has-[input:focus-visible]:ring-3 has-[input:focus-visible]:ring-ring/20",
          "has-[input[aria-invalid=true]]:border-destructive has-[input[aria-invalid=true]:focus-visible]:ring-destructive/20",
          "has-[input:disabled]:cursor-not-allowed has-[input:disabled]:bg-muted",
          className,
        )}
      >
        <span className="shrink-0 truncate pl-2.5 text-muted-foreground select-none">{prefix}</span>
        <input
          data-slot="input"
          type={type}
          className="h-full min-w-0 flex-1 bg-transparent pr-2.5 text-foreground outline-none placeholder:text-faint-foreground disabled:cursor-not-allowed disabled:text-faint-foreground"
          {...props}
        />
      </div>
    );
  }
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
