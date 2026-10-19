// What the hero keeps between visits, in this browser only (localStorage):
// the draft (SQL, layout, groups, discussion) and the versions saved. Never
// sent anywhere; "Reset" forgets it.

import type { DocComment, DocThread } from "@/features/comments/model";
import type { Groups, Layout } from "@/features/schema/layout";

import { SEED_GROUPS, SEED_LAYOUT, SEED_LAYOUT_NARROW, SEED_SQL, seedDiscussion } from "./seed";

export interface SavedVersion {
  number: number;
  sql: string;
  layout: Layout;
  groups: Groups;
  savedAt: string;
  by: string;
}

export interface Saved {
  sql: string;
  layout: Layout;
  groups: Groups;
  threads: Record<string, DocThread>;
  comments: Record<string, DocComment>;
  versions: SavedVersion[];
}

const KEY = "romulus:hero:1";

/** The demo as it starts; `narrow`: laid out for a phone's window. */
export function seed(narrow = false): Saved {
  const layout = narrow ? SEED_LAYOUT_NARROW : SEED_LAYOUT;
  return {
    sql: SEED_SQL,
    layout,
    groups: SEED_GROUPS,
    ...seedDiscussion(),
    versions: [
      {
        number: 1,
        sql: SEED_SQL,
        layout,
        groups: SEED_GROUPS,
        savedAt: new Date(Date.now() - 2 * 86_400_000).toISOString(),
        by: "Ada Moreau",
      },
    ],
  };
}

const isObject = (v: unknown): v is Record<string, unknown> =>
  typeof v === "object" && v !== null && !Array.isArray(v);
const isString = (v: unknown): v is string => typeof v === "string";
const isNumber = (v: unknown): v is number => typeof v === "number" && Number.isFinite(v);
const isTime = (v: unknown) => isString(v) && !Number.isNaN(Date.parse(v));
const isStrings = (v: unknown) => Array.isArray(v) && v.every(isString);
const every = (v: unknown, valid: (item: unknown) => boolean) =>
  isObject(v) && Object.values(v).every(valid);

const isPosition = (v: unknown) => isObject(v) && isNumber(v.x) && isNumber(v.y);
const isGroup = (v: unknown) =>
  isObject(v) && isString(v.name) && isString(v.color) && isStrings(v.members);

function isAnchor(v: unknown): boolean {
  if (!isObject(v)) return false;
  const statement = v.statement === undefined || isString(v.statement);
  switch (v.kind) {
    case "table":
      return isString(v.table) && statement;
    case "column":
      return isString(v.table) && isString(v.column) && statement;
    case "point":
      return isNumber(v.x) && isNumber(v.y);
    case "statement":
      return isString(v.statement);
    default:
      return false;
  }
}

const isThread = (v: unknown) =>
  isObject(v) &&
  isAnchor(v.anchor) &&
  isString(v.by) &&
  isTime(v.at) &&
  (v.resolved === null ||
    (isObject(v.resolved) &&
      isString(v.resolved.by) &&
      isString(v.resolved.name) &&
      isTime(v.resolved.at)));

const isComment = (v: unknown) =>
  isObject(v) &&
  isString(v.thread) &&
  isObject(v.author) &&
  isString(v.author.id) &&
  isString(v.author.name) &&
  isString(v.body) &&
  isStrings(v.mentions) &&
  isTime(v.at) &&
  (v.edited === null || isTime(v.edited));

const isVersion = (v: unknown, i: number) =>
  isObject(v) &&
  v.number === i + 1 &&
  isString(v.sql) &&
  every(v.layout, isPosition) &&
  every(v.groups, isGroup) &&
  isTime(v.savedAt) &&
  isString(v.by);

/** Every value of `state` has the shape the hero reads: anything else is not kept. */
function isSaved(v: unknown): v is Saved {
  return (
    isObject(v) &&
    isString(v.sql) &&
    every(v.layout, isPosition) &&
    every(v.groups, isGroup) &&
    every(v.threads, isThread) &&
    every(v.comments, isComment) &&
    // A comment always has its thread.
    every(v.comments, (c) => Object.hasOwn(v.threads as object, (c as DocComment).thread)) &&
    Array.isArray(v.versions) &&
    v.versions.length > 0 &&
    v.versions.every(isVersion)
  );
}

/**
 * What this browser kept, if every part of it reads as such; otherwise it is
 * forgotten (an older format, an edit by hand) and the demo starts over.
 */
export function load(): Saved | null {
  let raw: string | null;
  try {
    raw = localStorage.getItem(KEY);
  } catch {
    return null;
  }
  if (!raw) return null;
  try {
    const value: unknown = JSON.parse(raw);
    if (isSaved(value)) return value;
  } catch {
    // Not JSON.
  }
  clear();
  return null;
}

export function save(state: Saved) {
  try {
    localStorage.setItem(KEY, JSON.stringify(state));
  } catch {
    // Private mode or a full quota: the demo works for this visit only.
  }
}

export function clear() {
  try {
    localStorage.removeItem(KEY);
  } catch {
    // Nothing kept.
  }
}
