import { FileText } from "lucide-react";
import { Skeleton } from "@/components/ui/skeleton";
import { ReaderBody } from "@/components/reader/ReaderBody";
import { ReaderHeader } from "@/components/reader/ReaderHeader";
import { READER_SCROLL_ID } from "@/app/actions";
import { useAppState, useDispatch } from "@/app/store";
import { isStale, readerKey } from "@/app/state";

function DraftSummary({ id }: { id: string }) {
  const s = useAppState();
  const list = s.messages.data;
  const d = list?.kind === "drafts" ? list.listing.drafts.find((x) => x.id === id) : undefined;
  if (!d) return null;
  return (
    <div className="flex flex-col gap-3 px-5 py-4 text-sm">
      <h2 className="text-lg font-semibold">{d.subject || "(no subject)"}</h2>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
        <dt className="text-muted-foreground">To</dt>
        <dd>{d.to ?? "(no recipient)"}</dd>
        <dt className="text-muted-foreground">Status</dt>
        <dd>{d.ready ? "ready to send" : d.valid ? "draft, not ready" : "does not parse"}</dd>
        <dt className="text-muted-foreground">File</dt>
        <dd className="font-mono text-xs break-all">{d.path}</dd>
      </dl>
      <p className="text-muted-foreground">Drafts open in the editor with compose (M3).</p>
    </div>
  );
}

export function ReaderPane() {
  const s = useAppState();
  const dispatch = useDispatch();
  const { account, message, draft } = s.selection;
  const key = account && message ? readerKey(account, message.row_id) : null;
  const ready = key !== null && s.reader.key === key && s.reader.meta !== null;
  const loading = key !== null && (!ready || isStale(s.reader.load)) && !s.reader.load.error;

  return (
    <aside
      aria-label="Reader"
      data-pane="reader"
      className="flex h-full min-h-0 min-w-0 flex-col"
      onFocus={() => dispatch({ type: "pane_focused", pane: "reader" })}
    >
      <div
        id={READER_SCROLL_ID}
        tabIndex={0}
        data-roving="active"
        aria-label="Message"
        aria-busy={loading}
        className="min-h-0 flex-1 overflow-y-auto outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
      >
        {draft ? (
          <DraftSummary id={draft} />
        ) : !message ? (
          <div className="flex h-full flex-col items-center justify-center gap-2 p-6 text-center text-sm text-muted-foreground">
            <FileText aria-hidden="true" className="size-6" />
            <p>No message selected.</p>
            <p className="text-xs">j / k to move, Enter to read.</p>
          </div>
        ) : s.reader.load.error && s.reader.key === key ? (
          <p role="alert" className="p-5 text-sm text-destructive">
            This message did not load: {s.reader.load.error.message}
          </p>
        ) : ready && s.reader.meta && s.reader.text ? (
          <article aria-label={s.reader.meta.subject ?? "Message"}>
            <ReaderHeader meta={s.reader.meta} />
            <ReaderBody
              account={s.reader.meta.account}
              rowId={s.reader.meta.row_id}
              htmlUrl={s.reader.meta.html_url}
              text={s.reader.text.body}
            />
          </article>
        ) : (
          <div className="flex flex-col gap-3 p-5" aria-label="Loading message">
            <Skeleton className="h-6 w-2/3" />
            <Skeleton className="h-4 w-1/2" />
            <Skeleton className="h-4 w-1/3" />
            <Skeleton className="mt-4 h-40 w-full" />
          </div>
        )}
      </div>
    </aside>
  );
}
