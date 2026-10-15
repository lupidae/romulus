import type * as React from "react";

import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { cn } from "@/lib/utils";

import type { Role } from "../queries";
import { roleDescription, roleLabel, roles } from "../roles";

interface RoleSelectProps extends Omit<React.ComponentProps<typeof SelectTrigger>, "onChange"> {
  value: Role;
  onValueChange: (role: Role) => void;
  /**
   * Why only Admin can be picked (the organization's last admin): shown in
   * the list, under the roles it keeps out of reach.
   */
  lockedReason?: React.ReactNode;
}

/** Picks a role, saying in the list what each one may do. */
export function RoleSelect({
  value,
  onValueChange,
  lockedReason,
  className,
  ...trigger
}: RoleSelectProps) {
  return (
    <Select value={value} onValueChange={(role) => onValueChange(role as Role)}>
      <SelectTrigger className={cn("w-32", className)} {...trigger}>
        <SelectValue />
      </SelectTrigger>
      <SelectContent align="end" className="w-72">
        {roles.map((role) => (
          <SelectItem
            key={role}
            value={role}
            description={roleDescription[role]}
            disabled={lockedReason !== undefined && role !== "admin"}
          >
            {roleLabel[role]}
          </SelectItem>
        ))}
        {lockedReason && (
          <p className="mx-1 mt-1 border-t border-border px-1 pt-2 pb-1 text-xs text-muted-foreground">
            {lockedReason}
          </p>
        )}
      </SelectContent>
    </Select>
  );
}
