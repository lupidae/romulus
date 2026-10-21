import { useNavigate } from "@tanstack/react-router";

import {
  ComputerIcon,
  HalfMoonIcon,
  KeyIcon,
  LockIcon,
  LogOutIcon,
  SparksIcon,
  SunLightIcon,
} from "@/components/icons";
import { Avatar } from "@/components/ui/avatar";
import { Button } from "@/components/ui/button";
import {
  Menu,
  MenuContent,
  MenuItem,
  MenuLabel,
  MenuRadioGroup,
  MenuRadioItem,
  MenuSeparator,
  MenuTrigger,
} from "@/components/ui/dropdown-menu";
import { type ThemePreference, useTheme } from "@/lib/theme";

import type { ShellUser } from "./types";

export function UserMenu({
  user,
  onSignOut,
  onChangePassword,
}: {
  user: ShellUser;
  onSignOut: () => void;
  /** Password accounts only: opens "Change password". */
  onChangePassword?: () => void;
}) {
  const { preference, setPreference } = useTheme();
  const navigate = useNavigate();
  return (
    <Menu>
      <MenuTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          aria-label={`Account: ${user.name}`}
          className="rounded-full data-[state=open]:bg-accent"
        >
          <Avatar id={user.id} name={user.name} size="md" />
        </Button>
      </MenuTrigger>
      <MenuContent align="end" className="w-60">
        <div className="grid gap-0.5 px-2 py-1.5">
          <p className="truncate text-sm font-medium text-foreground">{user.name}</p>
          <p className="truncate text-sm text-muted-foreground">{user.email}</p>
        </div>
        <MenuSeparator />
        <MenuLabel>Theme</MenuLabel>
        <MenuRadioGroup
          value={preference}
          onValueChange={(value) => setPreference(value as ThemePreference)}
        >
          <MenuRadioItem value="system">
            <ComputerIcon />
            System
          </MenuRadioItem>
          <MenuRadioItem value="light">
            <SunLightIcon />
            Light
          </MenuRadioItem>
          <MenuRadioItem value="dark">
            <HalfMoonIcon />
            Dark
          </MenuRadioItem>
        </MenuRadioGroup>
        <MenuSeparator />
        <MenuItem onSelect={() => void navigate({ to: "/settings/agent" })}>
          <SparksIcon />
          Connect your agent
        </MenuItem>
        <MenuItem onSelect={() => void navigate({ to: "/settings/tokens" })}>
          <KeyIcon />
          Access tokens
        </MenuItem>
        {onChangePassword && (
          <MenuItem onSelect={onChangePassword}>
            <LockIcon />
            Change password…
          </MenuItem>
        )}
        <MenuItem onSelect={onSignOut}>
          <LogOutIcon />
          Sign out
        </MenuItem>
      </MenuContent>
    </Menu>
  );
}
