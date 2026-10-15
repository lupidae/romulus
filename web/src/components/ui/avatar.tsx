import { Avatar as AvatarPrimitive } from "radix-ui";

import { cn } from "@/lib/utils";

import { Tooltip } from "./tooltip";

// Static class names: Tailwind only generates classes it can see in the source.
const presence = [
  "bg-presence-1",
  "bg-presence-2",
  "bg-presence-3",
  "bg-presence-4",
  "bg-presence-5",
  "bg-presence-6",
  "bg-presence-7",
  "bg-presence-8",
] as const;

/**
 * A stable collaborator color (1–8) for a user id: the same person gets the
 * same color on their avatar, their text cursor and their graph pointer.
 * FNV-1a over the UTF-16 code units, so the server can compute it too.
 */
export function presenceIndex(seed: string): number {
  let hash = 0x811c9dc5;
  for (let i = 0; i < seed.length; i++) {
    hash ^= seed.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return ((hash >>> 0) % presence.length) + 1;
}

export function initials(name: string): string {
  const words = name.trim().split(/\s+/).filter(Boolean);
  const first = words[0]?.[0] ?? "?";
  const last = words.length > 1 ? (words[words.length - 1]?.[0] ?? "") : "";
  return (first + last).toUpperCase();
}

const sizes = {
  sm: "size-5 text-[0.5625rem]",
  md: "size-6 text-[0.625rem]",
  lg: "size-8 text-xs",
  xl: "size-10 text-sm",
} as const;

export interface AvatarProps {
  /** Shown as initials when there is no image, and as the image's alt text. */
  name: string;
  /** Seeds the color; the user id, so two people named Alex differ. */
  id?: string;
  src?: string;
  size?: keyof typeof sizes;
  className?: string;
}

export function Avatar({ name, id, src, size = "md", className }: AvatarProps) {
  return (
    <AvatarPrimitive.Root
      data-slot="avatar"
      className={cn(
        "relative inline-flex shrink-0 overflow-hidden rounded-full select-none",
        sizes[size],
        className,
      )}
    >
      {src && <AvatarPrimitive.Image src={src} alt={name} className="size-full object-cover" />}
      <AvatarPrimitive.Fallback
        delayMs={src ? 400 : 0}
        className={cn(
          "flex size-full items-center justify-center font-semibold text-presence-foreground",
          presence[presenceIndex(id ?? name) - 1],
        )}
      >
        <span aria-hidden>{size === "sm" ? initials(name).slice(0, 1) : initials(name)}</span>
        <span className="sr-only">{name}</span>
      </AvatarPrimitive.Fallback>
    </AvatarPrimitive.Root>
  );
}

export interface AvatarStackProps {
  people: { id: string; name: string; src?: string }[];
  /** Avatars shown before collapsing the rest into “+N”. */
  max?: number;
  size?: keyof typeof sizes;
  className?: string;
}

/** Who is here: overlapping avatars, each named in a tooltip, the rest as +N. */
export function AvatarStack({ people, max = 4, size = "md", className }: AvatarStackProps) {
  const shown = people.slice(0, max);
  const rest = people.slice(max);
  const label =
    rest.length > 0
      ? `${shown.map((p) => p.name).join(", ")} and ${rest.length} more`
      : shown.map((p) => p.name).join(", ");
  return (
    <div role="group" aria-label={label} className={cn("flex items-center -space-x-1", className)}>
      {shown.map((person) => (
        <Tooltip key={person.id} content={person.name}>
          <span tabIndex={-1} className="rounded-full ring-2 ring-card">
            <Avatar {...person} size={size} />
          </span>
        </Tooltip>
      ))}
      {rest.length > 0 && (
        <Tooltip content={rest.map((p) => p.name).join(", ")}>
          <span
            tabIndex={-1}
            className={cn(
              "inline-flex items-center justify-center rounded-full bg-muted font-medium text-muted-foreground ring-2 ring-card tabular",
              sizes[size],
            )}
          >
            +{rest.length}
          </span>
        </Tooltip>
      )}
    </div>
  );
}
