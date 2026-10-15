import type { Present } from "./queries";

/** "Alice, Bob editing · Carol watching". */
export function presenceLine(present: Present[]): string {
  const editing = present.filter((p) => p.editing).map((p) => p.name);
  const watching = present.filter((p) => !p.editing).map((p) => p.name);
  return [
    editing.length > 0 && `${editing.join(", ")} editing`,
    watching.length > 0 && `${watching.join(", ")} watching`,
  ]
    .filter(Boolean)
    .join(" · ");
}
