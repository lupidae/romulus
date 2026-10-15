import type { Present } from "./queries";

const list = new Intl.ListFormat("en", { style: "long", type: "conjunction" });

/** "Alice Martin and Bob Durand editing, Carol Lin watching". */
export function presenceLine(present: Present[]): string {
  const editing = present.filter((p) => p.editing).map((p) => p.name);
  const watching = present.filter((p) => !p.editing).map((p) => p.name);
  return [
    editing.length > 0 && `${list.format(editing)} editing`,
    watching.length > 0 && `${list.format(watching)} watching`,
  ]
    .filter(Boolean)
    .join(", ");
}
