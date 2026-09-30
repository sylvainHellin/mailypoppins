import { CalendarDays, Download, ExternalLink, Paperclip } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { formatSize } from "@/components/list/format";
import { messageItems, openItem } from "@/app/attachments";
import { useDispatch } from "@/app/store";
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

/**
 * From `message_html_meta`: an absent header is null and is not shown.
 * Each attachment opens with the system opener (`to`) or saves into a
 * directory (`ts`).
 */
export function ReaderHeader({ meta }: { meta: MessageMeta }) {
  const dispatch = useDispatch();
  const owner = { kind: "message" as const, account: meta.account, row_id: meta.row_id };
  const items = messageItems(meta.attachments);
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
      {items.length > 0 ? (
        <ul aria-label="Attachments" className="flex flex-wrap gap-2">
          {items.map((item) => (
            <li
              key={item.part}
              data-attachment={item.part}
              className="flex items-center gap-1.5 rounded-md border border-border bg-card py-0.5 pr-0.5 pl-2 text-xs"
            >
              <Paperclip aria-hidden="true" className="size-3.5 text-muted-foreground" />
              <span className="max-w-60 truncate">{item.name}</span>
              <span className="text-muted-foreground tabular-nums">{formatSize(item.size ?? 0)}</span>
              <Button
                size="icon-xs"
                variant="ghost"
                title="Open (t o)"
                aria-label={`Open ${item.name}`}
                onClick={() => void openItem(owner, item, dispatch)}
              >
                <ExternalLink aria-hidden="true" />
              </Button>
              <Button
                size="icon-xs"
                variant="ghost"
                title="Save (t s)"
                aria-label={`Save ${item.name}`}
                onClick={() =>
                  dispatch({
                    type: "open_attachments",
                    dialog: { kind: "save", account: meta.account, row_id: meta.row_id, subject: meta.subject ?? "(no subject)", items: [item] },
                  })
                }
              >
                <Download aria-hidden="true" />
              </Button>
            </li>
          ))}
        </ul>
      ) : null}
    </header>
  );
}
