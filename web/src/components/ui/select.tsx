import { Select as SelectPrimitive } from "radix-ui";
import * as React from "react";

import { CheckIcon, NavArrowDownIcon } from "@/components/icons";
import { cn } from "@/lib/utils";

import { controlClasses } from "./input";
import { usePortalContainer } from "./portal";

/** What a `Field` hands its one control. */
type FieldAttributes = Pick<
  React.ComponentProps<"button">,
  "id" | "aria-describedby" | "aria-invalid"
>;

const FieldOfTrigger = React.createContext<FieldAttributes>({});

/**
 * The root, which renders no element: inside a `Field`, the field's id and
 * aria attributes go on to the trigger, so the label names it.
 */
export function Select({
  id,
  "aria-describedby": describedBy,
  "aria-invalid": invalid,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Root> & FieldAttributes) {
  const field = React.useMemo(
    () => ({ id, "aria-describedby": describedBy, "aria-invalid": invalid }),
    [id, describedBy, invalid],
  );
  return (
    <FieldOfTrigger.Provider value={field}>
      <SelectPrimitive.Root {...props} />
    </FieldOfTrigger.Provider>
  );
}

export const SelectGroup = SelectPrimitive.Group;
export const SelectValue = SelectPrimitive.Value;

export function SelectTrigger({
  className,
  children,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Trigger>) {
  const field = React.useContext(FieldOfTrigger);
  return (
    <SelectPrimitive.Trigger
      data-slot="select-trigger"
      id={field.id}
      aria-describedby={field["aria-describedby"]}
      aria-invalid={field["aria-invalid"]}
      className={cn(
        controlClasses,
        "flex h-9 cursor-pointer items-center justify-between gap-2 px-2.5 text-left sm:h-8",
        "data-placeholder:text-faint-foreground [&>span]:truncate",
        className,
      )}
      {...props}
    >
      {children}
      <SelectPrimitive.Icon asChild>
        <NavArrowDownIcon className="size-4 shrink-0 text-muted-foreground" />
      </SelectPrimitive.Icon>
    </SelectPrimitive.Trigger>
  );
}

export function SelectContent({
  className,
  children,
  position = "popper",
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Content>) {
  return (
    <SelectPrimitive.Portal container={usePortalContainer()}>
      <SelectPrimitive.Content
        data-slot="select-content"
        position={position}
        sideOffset={4}
        className={cn(
          "relative z-(--z-popover) max-h-(--radix-select-content-available-height) min-w-32 overflow-hidden",
          "rounded-lg border border-border bg-popover text-popover-foreground shadow-popover",
          "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-[0.98]",
          "data-[state=closed]:animate-out data-[state=closed]:fade-out-0",
          position === "popper" && "w-full min-w-(--radix-select-trigger-width)",
          className,
        )}
        {...props}
      >
        <SelectPrimitive.Viewport className="p-1">{children}</SelectPrimitive.Viewport>
      </SelectPrimitive.Content>
    </SelectPrimitive.Portal>
  );
}

export function SelectLabel({
  className,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Label>) {
  return (
    <SelectPrimitive.Label
      className={cn("px-2 pt-1.5 pb-1 text-xs text-muted-foreground", className)}
      {...props}
    />
  );
}

/**
 * An option. `description` adds a muted line under the label in the list
 * (what a role allows, say); the trigger shows the label only.
 */
export function SelectItem({
  className,
  children,
  description,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Item> & { description?: React.ReactNode }) {
  return (
    <SelectPrimitive.Item
      data-slot="select-item"
      className={cn(
        "relative flex cursor-pointer rounded-md pr-8 pl-2 text-sm outline-none select-none",
        description ? "flex-col items-start gap-0.5 py-1.5" : "h-8 items-center",
        "data-highlighted:bg-highlight data-highlighted:text-accent-foreground",
        // Keyboard focus also gets a ring: a fill alone is too faint to track.
        "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
        "data-disabled:pointer-events-none data-disabled:opacity-45",
        className,
      )}
      {...props}
    >
      <SelectPrimitive.ItemText>{children}</SelectPrimitive.ItemText>
      {description && <span className="text-xs text-muted-foreground">{description}</span>}
      <SelectPrimitive.ItemIndicator className="absolute top-2 right-2 flex items-center">
        <CheckIcon className="size-4 text-primary" />
      </SelectPrimitive.ItemIndicator>
    </SelectPrimitive.Item>
  );
}

export function SelectSeparator({
  className,
  ...props
}: React.ComponentProps<typeof SelectPrimitive.Separator>) {
  return (
    <SelectPrimitive.Separator className={cn("-mx-1 my-1 h-px bg-border", className)} {...props} />
  );
}
