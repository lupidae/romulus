import { useMemo } from "react";

import { Combobox, type ComboboxOption } from "@/components/ui/combobox";

import { COMMON_TYPES, customType, modelTypes } from "../../edits/types";
import type { Schema } from "../../model";

interface TypePickerProps {
  schema: Schema;
  /** The text in the field. */
  value: string;
  onValueChange: (value: string) => void;
  id?: string;
  "aria-describedby"?: string;
  "aria-invalid"?: boolean;
  onKeyDown?: React.KeyboardEventHandler<HTMLInputElement>;
}

const GROUPS = { common: "Common", enum: "Enums", domain: "Domains", custom: "As typed" } as const;

/**
 * A column type: the common built-ins, the schema's enums and domains, or
 * any type name typed by hand. Read the choice with `typeFromText(schema, value)`.
 */
export function TypePicker({ schema, value, onValueChange, ...props }: TypePickerProps) {
  const choices = useMemo(() => [...COMMON_TYPES, ...modelTypes(schema)], [schema]);
  const options = useMemo<ComboboxOption[]>(() => {
    const query = value.trim().toLowerCase();
    const matching = choices.filter(
      (c) =>
        !query || c.label.toLowerCase().includes(query) || c.display.toLowerCase().includes(query),
    );
    // Prefix matches first.
    matching.sort(
      (a, b) =>
        Number(!a.label.toLowerCase().startsWith(query)) -
        Number(!b.label.toLowerCase().startsWith(query)),
    );
    const out: ComboboxOption[] = matching.map((c) => ({
      value: c.sql,
      label: c.label,
      description: c.hint,
      group: GROUPS[c.source],
    }));
    const typed = customType(value);
    if (typed && !choices.some((c) => c.sql === typed.sql))
      out.unshift({
        value: typed.sql,
        label: typed.sql,
        description: "Use as typed",
        group: GROUPS.custom,
      });
    return out;
  }, [choices, value]);

  return (
    <Combobox
      value={value}
      onValueChange={onValueChange}
      options={options}
      onPick={(option) => onValueChange(option.value)}
      monospace
      placeholder="text, bigint, uuid…"
      className="font-mono"
      empty="Not a type name. Try text, integer or varchar(80)."
      {...props}
    />
  );
}
