import { CalendarDays, Paperclip } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { formatSize } from "@/components/list/format";
import type { MessageMeta } from "@/lib/gui-types";

function Field({ name, value }: { name: string; value: string | null }) {
  if (value === null) return null;
  return (
    <>
      <dt className="text-muted-foreground">{name}</dt>
      <dd className="min-w-0 break-words">{value}</dd>
    </>
  );
}

/** From `message_html_meta`: an absent header is null and is not shown. */
export function ReaderHeader({ meta }: { meta: MessageMeta }) {
  return (
    <header className="flex flex-col gap-3 border-b border-border px-5 py-4">
      <h2 className="text-lg leading-snug font-semibold break-words">{meta.subject ?? "(no subject)"}</h2>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-sm">
        <Field name="From" value={meta.from} />
        <Field name="To" value={meta.to} />
        <Field name="Cc" value={meta.cc} />
        <Field name="Date" value={meta.date ?? "(no date)"} />
      </dl>
      {meta.flags.length > 0 || meta.invite ? (
        <ul aria-label="Flags" className="flex flex-wrap gap-1.5">
          {meta.flags.map((f) => (
            <li key={f}>
              <Badge variant="outline">{f}</Badge>
            </li>
          ))}
          {meta.invite ? (
            <li>
              <Badge variant="outline">
                <CalendarDays aria-hidden="true" />
                invitation
              </Badge>
            </li>
          ) : null}
        </ul>
      ) : null}
      {meta.attachments.length > 0 ? (
        <ul aria-label="Attachments" className="flex flex-wrap gap-2">
          {meta.attachments.map((a) => (
            <li
              key={a.name}
              className="flex items-center gap-1.5 rounded-md border border-border bg-card px-2 py-1 text-xs"
            >
              <Paperclip aria-hidden="true" className="size-3.5 text-muted-foreground" />
              <span className="max-w-60 truncate">{a.name}</span>
              <span className="text-muted-foreground tabular-nums">{formatSize(a.size)}</span>
            </li>
          ))}
        </ul>
      ) : null}
    </header>
  );
}
