import { CloudIcon, EyeIcon, TableIcon, ViewColumns3Icon } from "@/components/icons";

import type { TableKind } from "./build";

export const KIND_ICON: Record<TableKind, typeof TableIcon> = {
  table: TableIcon,
  partitioned: ViewColumns3Icon,
  view: EyeIcon,
  materialized: EyeIcon,
  foreign: CloudIcon,
};

/** What a node's badge says; plain tables say nothing. */
export const KIND_LABEL: Record<TableKind, string | null> = {
  table: null,
  partitioned: "partitioned",
  view: "view",
  materialized: "materialized view",
  foreign: "foreign table",
};
