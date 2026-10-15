const dateFormat = new Intl.DateTimeFormat(undefined, { month: "short", day: "numeric" });
const dateYearFormat = new Intl.DateTimeFormat(undefined, {
  month: "short",
  day: "numeric",
  year: "numeric",
});
const relative = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });

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
