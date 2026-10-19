import { CheckIcon, NavArrowDownIcon, PlusIcon } from "@/components/icons";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Menu,
  MenuContent,
  MenuGroup,
  MenuItem,
  MenuLabel,
  MenuSeparator,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { cn } from "@/lib/utils";

import type { ShellOrg } from "./types";

interface OrgSwitcherProps {
  current: ShellOrg;
  orgs: ShellOrg[];
  onSelect: (org: ShellOrg) => void;
  /** Absent where organizations cannot be created (a single-organization install). */
  onCreate?: () => void;
}

export function OrgTile({ name, className }: { name: string; className?: string }) {
  return (
    <span
      aria-hidden
      className={cn(
        "flex size-5 shrink-0 items-center justify-center rounded-sm bg-foreground text-[0.625rem] font-semibold text-background",
        className,
      )}
    >
      {name.trim()[0]?.toUpperCase() ?? "?"}
    </span>
  );
}

export function OrgSwitcher({ current, orgs, onSelect, onCreate }: OrgSwitcherProps) {
  return (
    <Menu>
      <MenuTrigger asChild>
        <Button
          variant="ghost"
          size="sm"
          aria-label={`Organization: ${current.name}. Switch organization`}
          className="max-w-56 gap-2 px-1.5 text-foreground data-[state=open]:bg-accent"
        >
          <OrgTile name={current.name} />
          <span className="truncate text-sm font-semibold">{current.name}</span>
          <NavArrowDownIcon className="text-muted-foreground" />
        </Button>
      </MenuTrigger>
      <MenuContent align="start" className="w-72">
        <MenuLabel>Organizations</MenuLabel>
        <MenuGroup>
          {orgs.map((org) => (
            <MenuItem key={org.id} onSelect={() => onSelect(org)} className="pr-2">
              <OrgTile name={org.name} />
              <span className="min-w-0 flex-1 truncate">{org.name}</span>
              <Badge variant="outline" className="capitalize">
                {org.role}
              </Badge>
              <CheckIcon
                aria-label={org.id === current.id ? "Current" : undefined}
                className={cn("text-primary!", org.id !== current.id && "invisible")}
              />
            </MenuItem>
          ))}
        </MenuGroup>
        {onCreate && (
          <>
            <MenuSeparator />
            <MenuItem onSelect={onCreate}>
              <PlusIcon />
              Create organization
            </MenuItem>
          </>
        )}
      </MenuContent>
    </Menu>
  );
}
