// Which rows a test can run on: a copy of a playground at the migration's
// base version (as built, not migrated), or fresh generated ones.

import { type Playground, formatCount, rowsTotal } from "@/features/playground/queries";

export type RowsChoice =
  { kind: "playground"; playground: Playground; mine: boolean } | { kind: "fresh" };

/** `fresh` or a playground's id: what a menu's radio group holds. */
export function choiceKey(choice: RowsChoice): string {
  return choice.kind === "fresh" ? "fresh" : choice.playground.id;
}

/**
 * The choices, the default first: the person's playground at `base` (the
 * draft's own before others), then other people's, then fresh rows.
 * Without a base (a database without versions), fresh rows only.
 */
export function rowsChoices(
  playgrounds: Playground[] | undefined,
  base: number | null | undefined,
  me: string | undefined,
  draft?: string,
): RowsChoice[] {
  const usable = (playgrounds ?? []).filter(
    (p) => base != null && p.status === "ready" && p.version === base && !p.migrated_by,
  );
  const rank = (p: Playground) =>
    (p.created_by?.id === me ? 0 : 2) + (draft && p.draft?.id === draft ? 0 : 1);
  const sorted = [...usable].sort((a, b) => rank(a) - rank(b));
  return [
    ...sorted.map((playground): RowsChoice => ({
      kind: "playground",
      playground,
      mine: playground.created_by?.id === me,
    })),
    { kind: "fresh" },
  ];
}

/** "Your playground", "Bob's playground", "Fresh generated rows". */
export function choiceLabel(choice: RowsChoice): string {
  if (choice.kind === "fresh") return "Fresh generated rows";
  if (choice.mine) return "Your playground";
  return `${choice.playground.created_by?.name ?? "Someone"}'s playground`;
}

/** "Small · 2,400 rows · seed 7f3a91", "Small rows, a new seed". */
export function choiceDetail(choice: RowsChoice): string {
  if (choice.kind === "fresh") return "Small, a new seed";
  const p = choice.playground;
  const size = p.size === "medium" ? "Medium" : "Small";
  return `${size} · ${formatCount(rowsTotal(p))} rows · seed ${p.seed}`;
}
