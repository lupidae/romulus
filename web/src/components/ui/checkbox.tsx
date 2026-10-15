import { Checkbox as CheckboxPrimitive } from "radix-ui";
import type * as React from "react";

import { CheckIcon } from "@/components/icons";
import { cn } from "@/lib/utils";

/** A box to tick: pair it with a Label, or give it an `aria-label`. */
export function Checkbox({
  className,
  ...props
}: React.ComponentProps<typeof CheckboxPrimitive.Root>) {
  return (
    <CheckboxPrimitive.Root
      data-slot="checkbox"
      className={cn(
        "peer relative inline-flex size-4 shrink-0 cursor-pointer items-center justify-center rounded-xs border border-border-strong bg-card shadow-xs",
        "transition-[background-color,border-color,box-shadow] outline-none",
        "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
        "data-[state=checked]:border-primary data-[state=checked]:bg-primary data-[state=checked]:text-primary-foreground",
        "disabled:cursor-not-allowed disabled:opacity-45",
        // Touch screens: a 44px hit area around the 16px box.
        "pointer-coarse:after:absolute pointer-coarse:after:top-1/2 pointer-coarse:after:left-1/2 pointer-coarse:after:size-11 pointer-coarse:after:-translate-1/2",
        className,
      )}
      {...props}
    >
      <CheckboxPrimitive.Indicator className="flex items-center justify-center [&_svg]:size-3 [&_svg]:stroke-[2.5]">
        <CheckIcon aria-hidden />
      </CheckboxPrimitive.Indicator>
    </CheckboxPrimitive.Root>
  );
}
