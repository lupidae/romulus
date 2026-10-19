import { DropdownMenu as MenuPrimitive } from "radix-ui";
import type * as React from "react";

import { CheckIcon, NavArrowRightIcon } from "@/components/icons";
import { cn } from "@/lib/utils";
import { usePortalContainer } from "./portal";

/**
 * Menus: account, overflow (⋯) and switchers. Fully keyboard driven (arrows,
 * typeahead, Esc). Put a shortcut hint in <MenuShortcut> when one exists.
 */
export const Menu = MenuPrimitive.Root;
export const MenuTrigger = MenuPrimitive.Trigger;
export const MenuGroup = MenuPrimitive.Group;
export const MenuRadioGroup = MenuPrimitive.RadioGroup;
export const MenuSub = MenuPrimitive.Sub;

const surface = cn(
  "z-(--z-popover) min-w-48 overflow-hidden rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-popover",
  "origin-(--radix-dropdown-menu-content-transform-origin)",
  "data-[state=open]:animate-in data-[state=open]:fade-in-0 data-[state=open]:zoom-in-[0.97]",
  "data-[state=closed]:animate-out data-[state=closed]:fade-out-0 data-[state=closed]:zoom-out-[0.97]",
);

const item = cn(
  "relative flex h-8 cursor-pointer items-center gap-2 rounded-md px-2 text-sm outline-none select-none",
  "data-highlighted:bg-highlight data-highlighted:text-accent-foreground",
  // Keyboard focus also gets a ring: a fill alone is too faint to track.
  "focus-visible:ring-2 focus-visible:ring-ring focus-visible:ring-inset",
  "data-disabled:pointer-events-none data-disabled:opacity-45",
  "[&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:text-muted-foreground",
);

export function MenuContent({
  className,
  sideOffset = 6,
  ...props
}: React.ComponentProps<typeof MenuPrimitive.Content>) {
  return (
    <MenuPrimitive.Portal container={usePortalContainer()}>
      <MenuPrimitive.Content
        data-slot="menu-content"
        sideOffset={sideOffset}
        className={cn(surface, className)}
        {...props}
      />
    </MenuPrimitive.Portal>
  );
}

/**
 * An action. `description` adds a muted line under the label, aligned with
 * it (put the icon first): for menus whose choices need a word of
 * explanation ("New draft" / "New migration").
 */
export function MenuItem({
  className,
  variant = "default",
  description,
  children,
  ...props
}: React.ComponentProps<typeof MenuPrimitive.Item> & {
  variant?: "default" | "destructive";
  description?: React.ReactNode;
}) {
  return (
    <MenuPrimitive.Item
      data-slot="menu-item"
      className={cn(
        item,
        description && "h-auto items-start py-1.5",
        variant === "destructive" &&
          "text-destructive data-highlighted:bg-destructive/10 data-highlighted:text-destructive [&_svg]:text-destructive",
        className,
      )}
      {...props}
    >
      {description ? (
        <span className="grid min-w-0 gap-0.5">
          <span className="flex items-center gap-2">{children}</span>
          <span className="pl-6 text-xs text-muted-foreground">{description}</span>
        </span>
      ) : (
        children
      )}
    </MenuPrimitive.Item>
  );
}

export function MenuCheckboxItem({
  className,
  children,
  ...props
}: React.ComponentProps<typeof MenuPrimitive.CheckboxItem>) {
  return (
    <MenuPrimitive.CheckboxItem className={cn(item, "pr-8", className)} {...props}>
      {children}
      <MenuPrimitive.ItemIndicator className="absolute right-2 flex items-center">
        <CheckIcon className="text-primary!" />
      </MenuPrimitive.ItemIndicator>
    </MenuPrimitive.CheckboxItem>
  );
}

export function MenuRadioItem({
  className,
  children,
  ...props
}: React.ComponentProps<typeof MenuPrimitive.RadioItem>) {
  return (
    <MenuPrimitive.RadioItem className={cn(item, "pr-8", className)} {...props}>
      {children}
      <MenuPrimitive.ItemIndicator className="absolute right-2 flex items-center">
        <CheckIcon className="text-primary!" />
      </MenuPrimitive.ItemIndicator>
    </MenuPrimitive.RadioItem>
  );
}

export function MenuLabel({
  className,
  ...props
}: React.ComponentProps<typeof MenuPrimitive.Label>) {
  return (
    <MenuPrimitive.Label
      className={cn("px-2 pt-1.5 pb-1 text-xs text-muted-foreground", className)}
      {...props}
    />
  );
}

export function MenuSeparator({
  className,
  ...props
}: React.ComponentProps<typeof MenuPrimitive.Separator>) {
  return (
    <MenuPrimitive.Separator className={cn("-mx-1 my-1 h-px bg-border", className)} {...props} />
  );
}

export function MenuShortcut({ className, ...props }: React.ComponentProps<"span">) {
  return (
    <span
      className={cn("ml-auto pl-4 text-xs tracking-wide text-faint-foreground", className)}
      {...props}
    />
  );
}

export function MenuSubTrigger({
  className,
  children,
  ...props
}: React.ComponentProps<typeof MenuPrimitive.SubTrigger>) {
  return (
    <MenuPrimitive.SubTrigger
      className={cn(item, "data-[state=open]:bg-accent", className)}
      {...props}
    >
      {children}
      <NavArrowRightIcon className="ml-auto" />
    </MenuPrimitive.SubTrigger>
  );
}

export function MenuSubContent({
  className,
  ...props
}: React.ComponentProps<typeof MenuPrimitive.SubContent>) {
  return (
    <MenuPrimitive.Portal container={usePortalContainer()}>
      <MenuPrimitive.SubContent sideOffset={6} className={cn(surface, className)} {...props} />
    </MenuPrimitive.Portal>
  );
}
