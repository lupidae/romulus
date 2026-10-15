import { cva, type VariantProps } from "class-variance-authority";
import { Slot } from "radix-ui";
import type * as React from "react";

import { cn } from "@/lib/utils";

import { Spinner } from "./spinner";

export const buttonVariants = cva(
  [
    "relative inline-flex shrink-0 cursor-pointer items-center justify-center gap-1.5 whitespace-nowrap select-none",
    "rounded-md font-medium transition-[background-color,border-color,color,box-shadow]",
    "outline-none focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-offset-2 focus-visible:ring-offset-background",
    "disabled:pointer-events-none disabled:opacity-45",
    "[&_svg]:pointer-events-none [&_svg]:shrink-0",
    // Touch screens: an invisible 44px hit area around small buttons, so the
    // dense desktop sizes stay tappable without growing on screen.
    "pointer-coarse:after:absolute pointer-coarse:after:top-1/2 pointer-coarse:after:left-1/2 pointer-coarse:after:size-full pointer-coarse:after:min-h-11 pointer-coarse:after:min-w-11 pointer-coarse:after:-translate-1/2",
  ],
  {
    variants: {
      variant: {
        primary:
          "bg-primary text-primary-foreground shadow-control hover:bg-primary-hover active:bg-primary-hover",
        secondary:
          "border border-border bg-card text-foreground shadow-control hover:border-border-strong hover:bg-secondary active:bg-secondary-hover",
        ghost: "text-muted-foreground hover:bg-accent hover:text-accent-foreground",
        destructive:
          "bg-destructive text-destructive-foreground shadow-control hover:bg-destructive-hover",
        link: "h-auto px-0 text-primary underline-offset-4 hover:underline",
      },
      size: {
        sm: "h-7 px-2.5 text-sm [&_svg]:size-3.5",
        md: "h-9 px-3 text-sm sm:h-8 [&_svg]:size-4",
        lg: "h-10 px-4 text-base [&_svg]:size-4",
        icon: "size-9 sm:size-8 [&_svg]:size-4",
        "icon-sm": "size-7 [&_svg]:size-3.5",
      },
    },
    compoundVariants: [{ variant: "link", className: "h-auto px-0" }],
    defaultVariants: { variant: "secondary", size: "md" },
  },
);

export interface ButtonProps
  extends React.ComponentProps<"button">, VariantProps<typeof buttonVariants> {
  /** Render the child (a Link, an <a>) with button styling instead of a <button>. */
  asChild?: boolean;
  /** Shows a spinner over the label and blocks clicks; the width does not change. */
  loading?: boolean;
}

/**
 * Variants: `primary` for the one main action of a view, `secondary` (default)
 * for everything else, `ghost` in toolbars and menus, `destructive` only to
 * confirm a destructive action, `link` inline in text.
 * Icon-only buttons need an `aria-label` (and usually a Tooltip).
 */
export function Button({
  className,
  variant,
  size,
  asChild = false,
  loading = false,
  disabled,
  type = "button",
  onClick,
  children,
  ...props
}: ButtonProps) {
  const Comp = asChild ? Slot.Root : "button";
  return (
    <Comp
      data-slot="button"
      className={cn(buttonVariants({ variant, size }), loading && "pointer-events-none", className)}
      {...(asChild ? {} : { type, disabled })}
      // Busy is not unavailable: a loading button keeps its color and only
      // stops taking clicks until the work is done.
      aria-busy={loading || undefined}
      aria-disabled={loading || undefined}
      onClick={loading ? (e: React.MouseEvent<HTMLButtonElement>) => e.preventDefault() : onClick}
      {...props}
    >
      {loading && !asChild ? (
        <>
          <span className="invisible contents">{children}</span>
          <span className="absolute inset-0 flex items-center justify-center">
            <Spinner />
          </span>
        </>
      ) : (
        children
      )}
    </Comp>
  );
}
