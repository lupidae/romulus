import { CloudIcon, EditPencilIcon, GitBranchIcon, KeyIcon } from "@/components/icons";
import { Avatar } from "@/components/ui/avatar";
import { Badge } from "@/components/ui/badge";
import { formatDateTime, timeAgo } from "@/lib/time";
import { cn } from "@/lib/utils";

import { type Author, authorName, originLabel, type VersionOrigin } from "../queries";

const originIcon = {
  upload: EditPencilIcon,
  push: CloudIcon,
  draft: GitBranchIcon,
} satisfies Record<VersionOrigin, unknown>;

const originHint: Record<VersionOrigin, string> = {
  upload: "Uploaded as SQL",
  push: "Pushed with an API token",
  draft: "Published from a draft",
};

/** How a version came to be, as a word with its icon. */
export function OriginBadge({ origin }: { origin: VersionOrigin }) {
  const Icon = originIcon[origin];
  return (
    <Badge variant="outline" title={originHint[origin]}>
      <Icon aria-hidden />
      {originLabel[origin]}
    </Badge>
  );
}

/** A person's avatar, or a key for an API token. */
export function AuthorMark({
  author,
  size = "sm",
}: {
  author?: Author | null;
  size?: "sm" | "md";
}) {
  if (author?.kind === "user") return <Avatar name={author.name} id={author.id} size={size} />;
  return (
    <span
      aria-hidden
      className={cn(
        "inline-flex shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground ring-1 ring-border ring-inset",
        size === "sm" ? "size-5 [&_svg]:size-3" : "size-6 [&_svg]:size-3.5",
      )}
    >
      <KeyIcon />
    </span>
  );
}

/** "Alice Martin · 3 hours ago", the time exact on hover. */
export function Byline({
  author,
  at,
  className,
}: {
  author?: Author | null;
  at: string;
  className?: string;
}) {
  return (
    <span
      className={cn("inline-flex min-w-0 items-center gap-1.5 text-muted-foreground", className)}
    >
      <AuthorMark author={author} />
      <span className="truncate">
        {author?.kind === "token" && <span className="sr-only">API token </span>}
        {authorName(author)}
      </span>
      <span aria-hidden>·</span>
      <time dateTime={at} title={formatDateTime(at)} className="shrink-0 tabular-nums">
        {timeAgo(at)}
      </time>
    </span>
  );
}
