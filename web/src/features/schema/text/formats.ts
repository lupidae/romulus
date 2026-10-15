import type { CodeLanguage } from "./codemirror";

export type TextFormat = CodeLanguage;

/** The SQL, and the other renderings of the same model (pre-rendered by remus). */
export interface SchemaTexts {
  sql: string;
  json?: string;
  dbml?: string;
  mermaid?: string;
}

export const TEXT_FORMATS: { id: TextFormat; label: string }[] = [
  { id: "sql", label: "SQL" },
  { id: "json", label: "JSON" },
  { id: "dbml", label: "DBML" },
  { id: "mermaid", label: "Mermaid" },
];
