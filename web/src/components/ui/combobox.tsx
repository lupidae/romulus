import { Popover as PopoverPrimitive } from "radix-ui";
import * as React from "react";

import { cn } from "@/lib/utils";

import { Input } from "./input";
import { usePortalContainer } from "./portal";

export interface ComboboxOption {
  value: string;
  label: string;
  /** One short line after the label, quieter. */
  description?: string;
  /** Options with the same group are listed under it, in the order given. */
  group?: string;
}

interface ComboboxProps extends Omit<
  React.ComponentProps<typeof Input>,
  "value" | "onChange" | "onSelect" | "prefix"
> {
  /** The text in the field. */
  value: string;
  onValueChange: (value: string) => void;
  /** What the list offers for this text (filter them yourself). */
  options: ComboboxOption[];
  /** An option was picked (Enter, a click). */
  onPick: (option: ComboboxOption) => void;
  /** Shown when nothing matches. */
  empty?: React.ReactNode;
  /** Labels in the code font (types, names). */
  monospace?: boolean;
  /** Select the text when the field takes the focus (to type over a current value). */
  selectOnFocus?: boolean;
}

/**
 * A text field with a list of suggestions under it: type to narrow, arrows
 * to move, Enter to pick, Esc to close the list. The field keeps the focus
 * (ARIA combobox), so it works inside a popover or a form. Text that matches
 * nothing stays as typed: the caller decides whether that is valid.
 */
export function Combobox({
  value,
  onValueChange,
  options,
  onPick,
  empty,
  monospace = false,
  selectOnFocus = false,
  className,
  onKeyDown,
  onFocus,
  onBlur,
  ...props
}: ComboboxProps) {
  const id = React.useId();
  const listId = `${id}-list`;
  const [open, setOpen] = React.useState(false);
  const [active, setActive] = React.useState(0);
  const list = React.useRef<HTMLDivElement>(null);
  const shown = open && (options.length > 0 || empty !== undefined);
  const current = Math.min(active, options.length - 1);

  React.useEffect(() => {
    if (!shown || current < 0) return;
    list.current?.querySelector(`[data-index="${current}"]`)?.scrollIntoView({ block: "nearest" });
  }, [shown, current]);

  const pick = (option: ComboboxOption) => {
    onPick(option);
    setOpen(false);
  };

  // Groups in first-seen order, options in the order given.
  const groups: { name: string | undefined; items: { option: ComboboxOption; index: number }[] }[] =
    [];
  options.forEach((option, index) => {
    const group = groups.find((g) => g.name === option.group);
    if (group) group.items.push({ option, index });
    else groups.push({ name: option.group, items: [{ option, index }] });
  });

  return (
    <PopoverPrimitive.Root open={shown} onOpenChange={setOpen}>
      <PopoverPrimitive.Anchor asChild>
        <Input
          role="combobox"
          aria-expanded={shown}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={shown && current >= 0 ? `${id}-${current}` : undefined}
          autoComplete="off"
          spellCheck={false}
          value={value}
          className={className}
          onChange={(event) => {
            onValueChange(event.target.value);
            setActive(0);
            setOpen(true);
          }}
          // The list opens on typing, a click or ↓: not on focus, where it
          // would cover what comes after the field.
          onFocus={(event) => {
            if (selectOnFocus) event.currentTarget.select();
            onFocus?.(event);
          }}
          onClick={() => setOpen(true)}
          onBlur={(event) => {
            // A click in the list keeps the focus here (see the options).
            setOpen(false);
            onBlur?.(event);
          }}
          onKeyDown={(event) => {
            if (event.key === "ArrowDown" || event.key === "ArrowUp") {
              event.preventDefault();
              if (!shown) {
                setOpen(true);
                return;
              }
              const step = event.key === "ArrowDown" ? 1 : -1;
              setActive(
                (i) => (Math.min(i, options.length - 1) + step + options.length) % options.length,
              );
              return;
            }
            if (event.key === "Enter" && shown && current >= 0 && options[current]) {
              event.preventDefault();
              pick(options[current]);
              return;
            }
            if (event.key === "Escape" && shown) {
              // Close the list only, not what the field is in.
              event.preventDefault();
              event.stopPropagation();
              setOpen(false);
              return;
            }
            onKeyDown?.(event);
          }}
          {...props}
        />
      </PopoverPrimitive.Anchor>
      <PopoverPrimitive.Portal container={usePortalContainer()}>
        <PopoverPrimitive.Content
          align="start"
          sideOffset={4}
          // The field keeps the focus.
          onOpenAutoFocus={(event) => event.preventDefault()}
          onCloseAutoFocus={(event) => event.preventDefault()}
          onInteractOutside={(event) => {
            // The field itself is outside the list: typing there must not close it.
            if (
              event.target instanceof Element &&
              event.target.closest(`[aria-controls="${listId}"]`)
            )
              event.preventDefault();
          }}
          className={cn(
            "z-(--z-popover) w-(--radix-popper-anchor-width) min-w-56 overflow-hidden rounded-lg border border-border bg-popover p-1 text-popover-foreground shadow-popover outline-none",
            "data-[state=open]:animate-in data-[state=open]:fade-in-0",
          )}
        >
          <div ref={list} id={listId} role="listbox" className="max-h-60 overflow-y-auto">
            {options.length === 0 && (
              <p className="px-2 py-1.5 text-sm text-muted-foreground">{empty}</p>
            )}
            {groups.map((group) => (
              <div
                key={group.name ?? ""}
                role={group.name ? "group" : undefined}
                aria-label={group.name}
              >
                {group.name && (
                  <div className="px-2 pt-1.5 pb-1 text-xs text-muted-foreground" aria-hidden>
                    {group.name}
                  </div>
                )}
                {group.items.map(({ option, index }) => (
                  // The field keeps the focus: options are picked by pointer
                  // or by the field's keys (aria-activedescendant).
                  // eslint-disable-next-line jsx-a11y/click-events-have-key-events
                  <div
                    key={`${option.group ?? ""}\u0000${option.value}`}
                    id={`${id}-${index}`}
                    data-index={index}
                    role="option"
                    aria-selected={index === current}
                    tabIndex={-1}
                    onMouseDown={(event) => event.preventDefault()}
                    onMouseMove={() => setActive(index)}
                    onClick={() => pick(option)}
                    className={cn(
                      "flex h-8 cursor-pointer items-center gap-2 rounded-md px-2 text-sm select-none",
                      index === current && "bg-highlight text-accent-foreground",
                    )}
                  >
                    <span
                      className={cn(
                        "min-w-0 truncate text-foreground",
                        monospace ? "font-mono text-xs" : "text-sm",
                      )}
                    >
                      {option.label}
                    </span>
                    {option.description && (
                      <span className="ml-auto min-w-0 truncate pl-3 text-xs text-muted-foreground">
                        {option.description}
                      </span>
                    )}
                  </div>
                ))}
              </div>
            ))}
          </div>
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  );
}
