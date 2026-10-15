import { LogoMark } from "@/components/brand/logo";
import {
  DatabaseIcon,
  GroupIcon,
  MailIcon,
  NavArrowDownIcon,
  SettingsIcon,
} from "@/components/icons";
import { OrgTile } from "@/components/shell/org-switcher";
import { Avatar } from "@/components/ui/avatar";
import { cn } from "@/lib/utils";

/**
 * The organization as it will look in romulus, drawn live from the
 * onboarding form: its address, its switcher, its navigation. Decorative
 * (the caller hides it from assistive tech); tokens only, so it follows the
 * theme.
 */
export function OrgPreview({
  name,
  slug,
  user,
  host,
}: {
  name: string;
  slug: string;
  user: { id: string; name: string };
  host: string;
}) {
  const shown = name.trim() || "Your organization";
  return (
    <div className="w-full max-w-lg overflow-hidden rounded-xl border border-border bg-background shadow-raised">
      <div className="flex h-9 items-center gap-3 border-b border-border bg-muted px-3">
        <div className="flex gap-1.5">
          {[0, 1, 2].map((i) => (
            <span key={i} className="size-2.5 rounded-full bg-border-strong" />
          ))}
        </div>
        <div className="flex h-6 min-w-0 flex-1 items-center rounded-md bg-card px-2 font-mono text-xs text-muted-foreground">
          <span className="truncate">
            {host}/app/
            <span className={cn("text-foreground", !slug && "text-faint-foreground")}>
              {slug || "your-org"}
            </span>
          </span>
        </div>
      </div>
      <div className="flex h-11 items-center justify-between px-3">
        <div className="flex min-w-0 items-center gap-2">
          <LogoMark className="size-5" />
          <span className="h-4 w-px rotate-12 bg-border-strong" />
          <OrgTile name={shown} className="transition-transform duration-(--duration-base)" />
          <span
            className={cn(
              "truncate text-sm font-semibold",
              name.trim() ? "text-foreground" : "text-faint-foreground",
            )}
          >
            {shown}
          </span>
          <NavArrowDownIcon className="size-3.5 shrink-0 text-muted-foreground" />
        </div>
        <Avatar id={user.id} name={user.name} size="md" />
      </div>
      <div className="flex h-56 gap-2 px-2 pb-2">
        <div className="grid w-32 content-start gap-0.5 py-1 text-xs font-medium text-sidebar-foreground">
          <PreviewNav icon={<DatabaseIcon />} label="Databases" active />
          <p className="px-2 pt-3 pb-1 text-faint-foreground">Settings</p>
          <PreviewNav icon={<SettingsIcon />} label="General" />
          <PreviewNav icon={<GroupIcon />} label="Members" />
          <PreviewNav icon={<MailIcon />} label="Invitations" />
        </div>
        <div className="grid flex-1 content-start gap-3 rounded-lg border border-border bg-card p-4">
          <span className="text-sm font-semibold text-foreground">Databases</span>
          <div className="grid gap-2">
            {[0.9, 0.7, 0.8].map((w, i) => (
              <div key={i} className="flex items-center gap-2">
                <span className="size-5 rounded-sm bg-muted" />
                <span className="h-2 rounded-full bg-muted" style={{ width: `${w * 60}%` }} />
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}

function PreviewNav({
  icon,
  label,
  active = false,
}: {
  icon: React.ReactNode;
  label: string;
  active?: boolean;
}) {
  return (
    <span
      className={cn(
        "flex h-6 items-center gap-1.5 rounded-md px-2 [&_svg]:size-3 [&_svg]:text-muted-foreground",
        active && "bg-sidebar-accent text-sidebar-accent-foreground",
      )}
    >
      {icon}
      {label}
    </span>
  );
}
