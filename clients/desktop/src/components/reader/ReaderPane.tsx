import { Cloud, CloudDownload, FileText, Forward, Globe, Reply, ReplyAll } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { DraftPreview } from "@/components/compose/DraftPreview";
import { ReaderBody, ReaderText } from "@/components/reader/ReaderBody";
import { ReaderHeader } from "@/components/reader/ReaderHeader";
import { InviteCard } from "@/components/reader/InviteCard";
import { ReaderToolbar } from "@/components/reader/ReaderToolbar";
import { InterceptedLinkNotice } from "@/components/reader/InterceptedLinkNotice";
import { READER_SCROLL_ID } from "@/app/actions";
import * as compose from "@/app/compose";
import { fetchHit, openHtml } from "@/app/attachments";
import { useAppState, useDispatch } from "@/app/store";
import { filteredDrafts, isStale, readerKey, type SearchHit } from "@/app/state";

/**
 * A server-only search hit: the store holds no row, so there is no body to
 * show, and a reply or a forward is built from the hit's own headers.
 * Fetch (`F`) downloads it into the store, after which the reader opens it;
 * its markup opens in the browser as it came.
 */
function ServerHitSummary({ hit }: { hit: SearchHit }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const fetching = s.fetching.includes(hit.key);
  const src = hit.source ? { kind: "hit" as const, account: hit.account, message: hit.source } : null;
  return (
    <article aria-label={hit.subject || "(no subject)"} className="flex flex-col gap-3 px-5 py-4 text-sm">
      <div role="toolbar" aria-label="Message actions" className="flex flex-wrap items-center gap-1">
        <Button size="sm" variant="ghost" title="Reply (r)" disabled={!src} onClick={() => compose.reply(src, false, dispatch)}>
          <Reply aria-hidden="true" />
          Reply
        </Button>
        <Button size="sm" variant="ghost" title="Reply all (ca)" disabled={!src} onClick={() => compose.reply(src, true, dispatch)}>
          <ReplyAll aria-hidden="true" />
          Reply all
        </Button>
        <Button size="sm" variant="ghost" title="Forward (cf)" disabled={!src} onClick={() => compose.forward(src, dispatch)}>
          <Forward aria-hidden="true" />
          Forward
        </Button>
        <Button
          size="sm"
          variant="ghost"
          title="Fetch into the local store (F)"
          disabled={fetching || hit.message_id === null}
          aria-busy={fetching || undefined}
          onClick={() => void fetchHit(s, dispatch)}
        >
          <CloudDownload aria-hidden="true" />
          {fetching ? "Fetching…" : "Fetch"}
        </Button>
        {hit.source?.html_body ? (
          <Button size="sm" variant="ghost" title="Open HTML in browser (t b)" onClick={() => void openHtml(s, dispatch)}>
            <Globe aria-hidden="true" />
            Open in browser
          </Button>
        ) : null}
      </div>
      <h2 className="text-lg font-semibold break-words">{hit.subject || "(no subject)"}</h2>
      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
        <dt className="text-muted-foreground">From</dt>
        <dd className="break-words">{hit.from || "(no sender)"}</dd>
        <dt className="text-muted-foreground">Date</dt>
        <dd>{hit.date_display || "(no date)"}</dd>
        <dt className="text-muted-foreground">Mailbox</dt>
        <dd>{hit.mailbox}</dd>
      </dl>
      <p className="flex items-center gap-2 text-muted-foreground">
        <Cloud aria-hidden="true" className="size-4" />
        This message is on the server only; the store has no copy to show. Fetch (F) downloads it with its attachments; a reply or a
        forward before that quotes it with none.
      </p>
    </article>
  );
}

export function ReaderPane() {
  const s = useAppState();
  const dispatch = useDispatch();
  const { account, message, draft } = s.selection;
  const draftRow = draft ? filteredDrafts(s.messages.data, "").find((d) => d.id === draft) : undefined;
  const serverHit = s.search && s.selection.hit ? s.search.hits.find((h) => h.key === s.selection.hit) : undefined;
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
        className="flex min-h-0 flex-1 flex-col overflow-y-auto outline-none focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-ring"
      >
        {draft && account ? (
          draftRow ? <DraftPreview key={`${account}/${draft}`} account={account} draft={draftRow} /> : null
        ) : serverHit ? (
          <ServerHitSummary hit={serverHit} />
        ) : !message ? (
          <div className="flex flex-1 flex-col items-center justify-center gap-2 p-6 text-center text-sm text-muted-foreground">
            <FileText aria-hidden="true" className="size-6" />
            <p>No message selected.</p>
            <p className="text-xs">j / k to move, Enter to read.</p>
          </div>
        ) : s.reader.load.error && s.reader.key === key ? (
          <p role="alert" className="p-5 text-sm text-destructive">
            This message did not load: {s.reader.load.error.message}
          </p>
        ) : ready && s.reader.meta ? (
          <article aria-label={s.reader.meta.subject ?? "Message"} className="flex flex-1 flex-col">
            <ReaderToolbar meta={s.reader.meta} />
            <ReaderHeader meta={s.reader.meta} />
            {s.reader.meta.invite ? <InviteCard meta={s.reader.meta} /> : null}
            {s.readerMode === "text" ? (
              <ReaderText
                account={s.reader.meta.account}
                rowId={s.reader.meta.row_id}
                version={s.reader.load.loadedGen}
                subject={s.reader.meta.subject}
              />
            ) : (
              <ReaderBody htmlUrl={s.reader.meta.html_url} subject={s.reader.meta.subject} />
            )}
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
      {s.interceptNotice ? <InterceptedLinkNotice entry={s.interceptNotice} /> : null}
    </aside>
  );
}
