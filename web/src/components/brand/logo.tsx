import { cn } from "@/lib/utils";

/** The romulus mark: an “r” whose arm ends in a graph node. */
export function LogoMark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" aria-hidden className={cn("size-6 shrink-0", className)}>
      <rect width="32" height="32" rx="8" className="fill-primary" />
      <path
        d="M11.5 23V13.75A4.75 4.75 0 0 1 16.25 9H17"
        fill="none"
        strokeWidth="2.75"
        strokeLinecap="round"
        className="stroke-primary-foreground"
      />
      <circle cx="21.5" cy="9" r="2.4" className="fill-primary-foreground" />
    </svg>
  );
}

export function Wordmark({ className }: { className?: string }) {
  return (
    <span className={cn("inline-flex items-center gap-2", className)}>
      <LogoMark />
      <span className="text-lg font-semibold tracking-tight text-foreground">romulus</span>
    </span>
  );
}
