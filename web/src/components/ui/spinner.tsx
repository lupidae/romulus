import { cn } from "@/lib/utils";

/** An indeterminate spinner. Sized by font-size, colored by currentColor. */
export function Spinner({ className, label = "Loading" }: { className?: string; label?: string }) {
  return (
    <svg
      viewBox="0 0 16 16"
      width="1em"
      height="1em"
      role="status"
      aria-label={label}
      className={cn("shrink-0 animate-spin", className)}
    >
      <circle
        cx="8"
        cy="8"
        r="6.25"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        opacity="0.25"
      />
      <path
        d="M14.25 8A6.25 6.25 0 0 0 8 1.75"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
      />
    </svg>
  );
}
