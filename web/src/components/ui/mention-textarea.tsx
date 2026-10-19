import { Popover as PopoverPrimitive } from "radix-ui";
import * as React from "react";

import { cn } from "@/lib/utils";

import { Avatar } from "./avatar";
import { Textarea } from "./input";
import { usePortalContainer } from "./portal";

/** Someone who can be @mentioned. */
export interface MentionPerson {
  id: string;
  name: string;
  /** One short line after the name (their email, their role). */
  detail?: string;
}

/** The `@name` being typed just before `caret`: where it starts and what is typed so far. */
export function mentionQuery(text: string, caret: number): { from: number; query: string } | null {
  const match = /(^|[\s(])@([^\s@]{0,40})$/.exec(text.slice(0, caret));
  if (!match) return null;
  return { from: caret - match[2]!.length - 1, query: match[2]! };
}

/** People whose name matches what is typed: names starting with it first. */
export function matchPeople(
  people: readonly MentionPerson[],
  query: string,
  limit = 8,
): MentionPerson[] {
  const q = query.toLowerCase();
  const starts = people.filter((p) => p.name.toLowerCase().startsWith(q));
  const contains = people.filter(
    (p) => !p.name.toLowerCase().startsWith(q) && p.name.toLowerCase().includes(q),
  );
  return [...starts, ...contains].slice(0, limit);
}

interface MentionTextareaProps extends Omit<
  React.ComponentProps<"textarea">,
  "value" | "onChange" | "onSubmit"
> {
  value: string;
  onValueChange: (value: string) => void;
  /** Who `@` suggests. */
  people: readonly MentionPerson[];
  /** Enter (without Shift, and no suggestion open) sends; Shift+Enter is a new line. */
  onSubmit?: () => void;
  /** Takes the focus when it appears, the caret at the end (a field the person just asked for). */
  focusOnMount?: boolean;
}

/**
 * A text area where `@` suggests people: type to narrow, ↑↓ to move, Enter
 * or Tab to insert `@Name`, Esc to close the list. The area keeps the focus
 * (ARIA combobox). With `onSubmit`, Enter sends.
 */
export function MentionTextarea({
  value,
  onValueChange,
  people,
  onSubmit,
  focusOnMount = false,
  className,
  onKeyDown,
  onBlur,
  ...props
}: MentionTextareaProps) {
  const id = React.useId();
  const listId = `${id}-list`;
  const area = React.useRef<HTMLTextAreaElement>(null);
  const [query, setQuery] = React.useState<{ from: number; query: string } | null>(null);
  const [active, setActive] = React.useState(0);
  const options = React.useMemo(
    () => (query ? matchPeople(people, query.query) : []),
    [people, query],
  );
  const shown = query !== null && options.length > 0;
  React.useLayoutEffect(() => {
    const el = area.current;
    if (!focusOnMount || !el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
    // Only as it appears.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  const current = Math.min(active, options.length - 1);
  // After a pick, the caret goes after the name as soon as the text has it:
  // before the next key (a frame later, keys typed meanwhile would land
  // before the caret is moved back).
  const caretAfterPick = React.useRef<number | null>(null);
  React.useLayoutEffect(() => {
    const el = area.current;
    const at = caretAfterPick.current;
    if (!el || at === null || el.value !== value) return;
    caretAfterPick.current = null;
    el.focus();
    el.setSelectionRange(at, at);
  }, [value]);

  const look = (text: string, caret: number) => {
    const found = mentionQuery(text, caret);
    setQuery(found);
    if (found?.query !== query?.query) setActive(0);
  };

  const pick = (person: MentionPerson) => {
    const el = area.current;
    if (!el || !query) return;
    const caret = el.selectionStart;
    const insert = `@${person.name} `;
    const next = value.slice(0, query.from) + insert + value.slice(caret);
    onValueChange(next);
    setQuery(null);
    caretAfterPick.current = query.from + insert.length;
  };

  return (
    <PopoverPrimitive.Root open={shown} onOpenChange={(open) => !open && setQuery(null)}>
      <PopoverPrimitive.Anchor asChild>
        <Textarea
          ref={area}
          role="combobox"
          aria-expanded={shown}
          aria-controls={listId}
          aria-autocomplete="list"
          aria-activedescendant={shown ? `${id}-${current}` : undefined}
          value={value}
          className={className}
          onChange={(event) => {
            onValueChange(event.target.value);
            look(event.target.value, event.target.selectionStart);
          }}
          onSelect={(event) => look(event.currentTarget.value, event.currentTarget.selectionStart)}
          onBlur={(event) => {
            setQuery(null);
            onBlur?.(event);
          }}
          onKeyDown={(event) => {
            if (shown) {
              if (event.key === "ArrowDown" || event.key === "ArrowUp") {
                event.preventDefault();
                const step = event.key === "ArrowDown" ? 1 : -1;
                setActive((current + step + options.length) % options.length);
                return;
              }
              if ((event.key === "Enter" || event.key === "Tab") && !event.shiftKey) {
                event.preventDefault();
                pick(options[current]!);
                return;
              }
              if (event.key === "Escape") {
                // Close the list only, not what the field is in.
                event.preventDefault();
                event.stopPropagation();
                setQuery(null);
                return;
              }
            }
            if (
              onSubmit &&
              event.key === "Enter" &&
              !event.shiftKey &&
              !event.nativeEvent.isComposing
            ) {
              event.preventDefault();
              onSubmit();
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
          side="bottom"
          sideOffset={4}
          onOpenAutoFocus={(event) => event.preventDefault()}
          onCloseAutoFocus={(event) => event.preventDefault()}
          onInteractOutside={(event) => {
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
          <div
            id={listId}
            role="listbox"
            aria-label="People to mention"
            className="max-h-60 overflow-y-auto"
          >
            {options.map((person, index) => (
              // The area keeps the focus: people are picked by pointer or by
              // its keys (aria-activedescendant).
              // eslint-disable-next-line jsx-a11y/click-events-have-key-events
              <div
                key={person.id}
                id={`${id}-${index}`}
                role="option"
                aria-selected={index === current}
                tabIndex={-1}
                onMouseDown={(event) => event.preventDefault()}
                onMouseMove={() => setActive(index)}
                onClick={() => pick(person)}
                className={cn(
                  "flex h-8 cursor-pointer items-center gap-2 rounded-md px-2 text-sm select-none",
                  index === current && "bg-highlight text-accent-foreground",
                )}
              >
                <Avatar name={person.name} id={person.id} size="sm" />
                <span className="min-w-0 truncate text-foreground">{person.name}</span>
                {person.detail && (
                  <span className="ml-auto min-w-0 truncate pl-3 text-xs text-muted-foreground">
                    {person.detail}
                  </span>
                )}
              </div>
            ))}
          </div>
        </PopoverPrimitive.Content>
      </PopoverPrimitive.Portal>
    </PopoverPrimitive.Root>
  );
}
