// Display helpers for list rows; pure, so rows stay pure. A sender's name
// and address are read in `@/lib/mailbox`.

const sameDay = (a: Date, b: Date) =>
  a.getFullYear() === b.getFullYear() && a.getMonth() === b.getMonth() && a.getDate() === b.getDate();

/**
 * `date_sort` is the UTC sort key (`2026-09-29T09:12:00`): today shows the
 * time, this year the day and month, older mail the year too.
 */
export function shortDate(dateSort: string, now: Date = new Date()): string {
  if (!dateSort) return "";
  const d = new Date(/[zZ]|[+-]\d\d:?\d\d$/.test(dateSort) ? dateSort : `${dateSort}Z`);
  if (Number.isNaN(d.getTime())) return "";
  if (sameDay(d, now)) return d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
  if (d.getFullYear() === now.getFullYear()) {
    return d.toLocaleDateString(undefined, { day: "numeric", month: "short" });
  }
  return d.toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

export function formatSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(0)} KB`;
  return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
}
