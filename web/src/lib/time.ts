const dateFormat = new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric" });
const dateYearFormat = new Intl.DateTimeFormat("en-US", {
  month: "short",
  day: "numeric",
  year: "numeric",
});
const relative = new Intl.RelativeTimeFormat("en-US", { numeric: "auto" });

/** "Oct 15", with the year only when it is not this year. */
export function formatDate(iso: string, now = new Date()): string {
  const date = new Date(iso);
  return (date.getFullYear() === now.getFullYear() ? dateFormat : dateYearFormat).format(date);
}

/** "in 6 days", "tomorrow", "in 3 hours": how far `iso` is from now. */
export function fromNow(iso: string, now = new Date()): string {
  const ms = new Date(iso).getTime() - now.getTime();
  const hours = ms / 3_600_000;
  if (Math.abs(hours) < 1) return relative.format(Math.round(ms / 60_000), "minute");
  if (Math.abs(hours) < 24) return relative.format(Math.round(hours), "hour");
  return relative.format(Math.round(hours / 24), "day");
}

/** "just now", "5 minutes ago", "yesterday", "3 days ago"; past `iso` times only. */
export function timeAgo(iso: string, now = new Date()): string {
  if (now.getTime() - new Date(iso).getTime() < 60_000) return "just now";
  return fromNow(iso, now);
}

const fullFormat = new Intl.DateTimeFormat("en-US", { dateStyle: "medium", timeStyle: "short" });

/** "Oct 15, 2026, 2:05 PM": the exact time, for a tooltip or `title`. */
export function formatDateTime(iso: string): string {
  return fullFormat.format(new Date(iso));
}
