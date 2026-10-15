import { type ClassValue, clsx } from "clsx";
import { extendTailwindMerge } from "tailwind-merge";

// tailwind-merge must know the custom scales, or it drops `text-sm` when a
// `text-muted-foreground` follows it (both look like `text-*` to it).
const twMerge = extendTailwindMerge({
  extend: {
    classGroups: {
      "font-size": [{ text: ["xs", "sm", "base", "lg", "xl", "2xl", "display"] }],
      shadow: [{ shadow: ["control", "xs", "raised", "popover", "dialog"] }],
    },
  },
});

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}
