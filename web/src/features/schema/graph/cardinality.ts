import { type Constraint, type Entity, isUniqueSet } from "../model";

/** Crow's foot ends in remus's (and Mermaid's) notation. */
export interface Cardinality {
  /** `||` exactly one parent, `|o` zero or one. */
  parent: "||" | "|o";
  /** `o|` zero or one child, `o{` zero or many. Never `||` or `|{`. */
  child: "o|" | "o{";
}

/**
 * What the catalog proves about `parent <-> child` (remus-mermaid's rule).
 *
 * Parent end: a foreign key targets a unique key, so a child row has at most
 * one parent; it has exactly one only when every FK column is NOT NULL.
 * Child end: a parent has at most one child only when the FK columns are
 * themselves unique. Nothing in the catalog says a parent must have a child,
 * so the child end is always "zero or".
 */
export function cardinality(child: Entity, fk: Constraint): Cardinality {
  const columns = fk.columns ?? [];
  const mandatory = columns.every(
    (name) => child.columns?.find((c) => c.name === name)?.not_null === true,
  );
  return {
    parent: mandatory ? "||" : "|o",
    child: isUniqueSet(child, columns) ? "o|" : "o{",
  };
}

/** The relationship in words, for an edge's accessible description and tooltips. */
export function describeCardinality({ parent, child }: Cardinality): string {
  const parentSide = parent === "||" ? "exactly one" : "zero or one";
  const childSide = child === "o|" ? "zero or one" : "zero or many";
  return `each row references ${parentSide}, each referenced row has ${childSide}`;
}
