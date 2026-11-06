// The reader pane for a draft: `draft_preview`'s record (the headers, the
// status and the body the dry run renders), `draft_validate`'s report and
// the `attachments:` list `draft_attachments` reads from the file, read
// again whenever the row or the file's version (`draftRevision`, a
// `draft.changed` naming this draft) moves; a reload of the list for any
// other reason reads nothing, and a re-read keeps the last preview up until
// it lands, so a sync never flashes the skeleton (PERSO-272).
// The body is the Markdown as written; `tt` switches it to the HTML a send
// would build (`draft.html`, PERSO-136) in the reader's sandboxed frame and
// back, through `state.draftHtml`, whatever the reader mode is (docs/reader.md,
// "Drafts and server-only hits"). The frame is fetched again with the rest
// when the row or the version moves, and every draft opens on its Markdown.
// `ComposeSummary` is the same preview after an embedded editor exited with
// 0 on a draft the selection does not show.

import { useEffect, useState } from "react";
import { CircleAlert, CircleCheck, ExternalLink, FilePen, Paperclip, Stamp, Undo2, Users, X } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { runAction } from "@/app/actions";
import { openInEditor, setShownStatus } from "@/app/compose";
import { draftItemsOf, openItem, removeItem } from "@/app/attachments";
import { ReaderBody, draftReaderUrl } from "@/components/reader/ReaderBody";
import { useAppState, useDispatch } from "@/app/store";
import { draftRevision, draftsShown, filteredDrafts, targetKey, type DraftItem } from "@/app/state";
import * as cmd from "@/lib/commands";
import { asGuiError, type DraftAttachments } from "@/lib/gui-types";
import { joinRead } from "@/lib/inflight";
import type { DraftPreview as Preview, DraftReport } from "@/protocol/types";

type Loaded = {
  /** The draft the answer is about, `account/id`: a re-read of the same draft shows it until its own answer lands. */
  draft: string;
  key: string;
  preview: Preview | null;
  report: DraftReport | null;
  attachments: DraftAttachments | null;
  error: string | null;
};

function Header({ label, value }: { label: string; value: string | null }) {
  if (!value) return null;
  return (
    <>
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="break-words">{value}</dd>
    </>
  );
}

/** A draft's status: `draft`, `approved`, or `invalid` for a file that does not parse. */
export function StatusPill({ status, hidden }: { status: string; hidden?: boolean }) {
  const variant = status === "invalid" ? "destructive" : status === "approved" ? "default" : "outline";
  return (
    <Badge variant={variant} data-slot="draft-status" data-status={status} aria-hidden={hidden || undefined}>
      {status}
    </Badge>
  );
}

/**
 * The selected draft, in place of a message body. `standalone` is the exit
 * summary of a draft the Drafts list may not show: its actions are Edit,
 * on the draft itself, and Close, which brings the selection back. `member`
 * is a conversation's draft member (PERSO-146): the Drafts actions less
 * Attach file, which stays in Drafts.
 */
export function DraftPreview({
  account,
  draft,
  standalone = false,
  member = false,
}: {
  account: string;
  draft: DraftItem;
  standalone?: boolean;
  member?: boolean;
}) {
  const s = useAppState();
  const dispatch = useDispatch();
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  // A row that changed (an approve) or a file written again (a body-only
  // save, a server sync) is read again; a list reload alone is neither.
  const shown = `${account}/${draft.id}`;
  const key = `${shown}|${draft.path}|${draft.status}|${draft.subject}|${draft.to}|${draft.valid}|${draftRevision(s, account, draft.id)}`;
  const invalid = draft.diagnostic !== null || !draft.valid;
  const showsHtml = s.draftHtml === targetKey({ account, draft: draft.id });

  // The next draft, or this one shown again, opens on its Markdown.
  useEffect(() => () => dispatch({ type: "draft_view_reset" }), [dispatch]);

  useEffect(() => {
    if (invalid) return;
    let live = true;
    // StrictMode's remount joins the reads on their way.
    joinRead(`draft_preview:${key}`, () =>
      Promise.allSettled([
        cmd.draftPreview(account, draft.id),
        cmd.draftValidate(account, draft.id),
        cmd.draftAttachments(account, draft.id),
      ]),
    ).then(([p, v, a]) => {
      if (!live) return;
      setLoaded({
        draft: shown,
        key,
        preview: p.status === "fulfilled" ? p.value : null,
        report: v.status === "fulfilled" ? (v.value.reports.find((r) => r.id === draft.id) ?? null) : null,
        attachments: a.status === "fulfilled" ? a.value : null,
        error: p.status === "rejected" ? asGuiError(p.reason).message : null,
      });
    });
    return () => {
      live = false;
    };
    // `key` carries every field the effect reads.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, invalid]);

  const pending = targetKey({ account, draft: draft.id }) in s.pending;
  const editing = targetKey({ account, draft: draft.id }) in s.compose;
  // The last answer about this draft, until the re-read's lands (PERSO-272).
  const last = loaded?.draft === shown ? loaded : null;
  const p = last?.preview ?? null;
  const report = last?.report ?? null;
  const subject = p?.subject || draft.subject || (invalid ? draft.id : "(no subject)");
  const status = draft.status;
  const attachments = last?.attachments ? draftItemsOf(last.attachments) : [];
  const owner = { kind: "draft" as const, account, draftId: draft.id };

  return (
    <article aria-label={`Draft: ${subject}`} data-slot="draft-preview" className="flex flex-1 flex-col gap-3 px-5 py-4 text-sm">
      <div role="toolbar" aria-label="Draft actions" aria-busy={pending || undefined} className="flex flex-wrap items-center gap-1">
        <Button
          size="sm"
          variant="ghost"
          title={standalone ? "Edit in the editor" : "Edit in the editor (e)"}
          onClick={() => (standalone ? void openInEditor(dispatch, account, draft.id, draft.path, s.compose) : runAction("open_editor", s, dispatch))}
        >
          <FilePen aria-hidden="true" />
          {editing ? "Reopen in editor" : "Edit in editor"}
        </Button>
        {standalone ? (
          <Button size="sm" variant="ghost" title="Show the selection again" onClick={() => dispatch({ type: "compose_summary_closed" })}>
            <X aria-hidden="true" />
            Close
          </Button>
        ) : invalid ? null : (
          <>
            <Button size="sm" variant="ghost" title="Edit recipients (ce)" onClick={() => runAction("edit_recipients", s, dispatch)}>
              <Users aria-hidden="true" />
              Edit recipients
            </Button>
            {member ? null : (
              <Button size="sm" variant="ghost" title="Attach file (ta)" onClick={() => runAction("attach_file", s, dispatch)}>
                <Paperclip aria-hidden="true" />
                Attach file
              </Button>
            )}
            {status === "approved" ? (
              <Button size="sm" variant="ghost" title="Back to draft (cD)" onClick={() => setShownStatus(s, dispatch, account, draft.id, false)}>
                <Undo2 aria-hidden="true" />
                Back to draft
              </Button>
            ) : (
              <Button size="sm" variant="ghost" title="Approve (cA)" onClick={() => setShownStatus(s, dispatch, account, draft.id, true)}>
                <Stamp aria-hidden="true" />
                Approve
              </Button>
            )}
          </>
        )}
      </div>
      <div className="flex items-center gap-2">
        <h2 className="min-w-0 flex-1 text-lg font-semibold break-words">{subject}</h2>
        <StatusPill status={invalid ? "invalid" : status} />
      </div>
      {invalid ? (
        <div data-slot="draft-diagnostic" className="flex flex-col gap-1 rounded-lg border border-border bg-card p-3">
          <p className="flex items-center gap-2 font-medium text-destructive">
            <CircleAlert aria-hidden="true" className="size-4" />
            This draft does not parse
          </p>
          <p className="break-words">{draft.diagnostic ?? "The file's frontmatter does not read."}</p>
          <p className="text-muted-foreground">Fix it in the editor; the list follows each save.</p>
        </div>
      ) : last?.error ? (
        <p data-slot="draft-diagnostic" className="text-destructive">
          The preview did not load: {last.error}
        </p>
      ) : !p ? (
        <div className="flex flex-col gap-2" aria-busy="true" aria-label="Loading the draft">
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-4 w-1/3" />
          <Skeleton className="mt-2 h-32 w-full" />
        </div>
      ) : (
        <>
          <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1">
            <Header label="From" value={p.from} />
            <Header label="To" value={p.to} />
            <Header label="Cc" value={p.cc} />
            <Header label="Bcc" value={p.bcc} />
          </dl>
          <div data-slot="draft-validation" className="flex flex-col gap-1">
            {report?.error ?? p.error ? (
              <p className="flex items-center gap-2 text-destructive">
                <CircleAlert aria-hidden="true" className="size-4 shrink-0" />
                {`Not sendable: ${report?.error ?? p.error}`}
              </p>
            ) : (
              <p className="flex items-center gap-2 text-muted-foreground">
                <CircleCheck aria-hidden="true" className="size-4 shrink-0 text-link" />
                Valid
              </p>
            )}
            {(report?.warnings ?? p.warnings).length > 0 ? (
              <ul aria-label="Warnings" className="list-disc pl-6 text-warning">
                {(report?.warnings ?? p.warnings).map((w) => (
                  <li key={w}>{w}</li>
                ))}
              </ul>
            ) : null}
          </div>
          {attachments.length > 0 ? (
            <ul aria-label="Attachments" data-slot="draft-attachments" className="flex flex-col gap-1">
              {attachments.map((item) => (
                <li
                  key={`${item.part}:${item.name}`}
                  data-attachment={item.part}
                  className="flex items-center gap-2 rounded-md border border-border bg-card py-0.5 pr-0.5 pl-2 text-xs"
                >
                  <Paperclip aria-hidden="true" className="size-3.5 shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate font-mono">{item.name}</span>
                  {item.missing ? (
                    <Badge variant="destructive" data-slot="attachment-missing">
                      missing
                    </Badge>
                  ) : null}
                  <Button
                    size="icon-xs"
                    variant="ghost"
                    title="Open (t o)"
                    aria-label={`Open ${item.name}`}
                    disabled={item.missing}
                    onClick={() => void openItem(owner, item, dispatch)}
                  >
                    <ExternalLink aria-hidden="true" />
                  </Button>
                  <Button
                    size="icon-xs"
                    variant="ghost"
                    title="Remove from the draft"
                    aria-label={`Remove ${item.name}`}
                    onClick={() =>
                      void removeItem(account, draft.id, item, dispatch).then(
                        (list) => list && setLoaded((l) => (l && l.draft === shown ? { ...l, attachments: list } : l)),
                      )
                    }
                  >
                    <X aria-hidden="true" />
                  </Button>
                </li>
              ))}
            </ul>
          ) : null}
          {showsHtml ? (
            // Keyed on the row and the version, so a save fetches the rendition again and a list reload does not.
            <ReaderBody key={key} htmlUrl={draftReaderUrl(account, draft.id)} subject={subject} label="Draft as sent" />
          ) : (
            <pre data-slot="draft-body" className="font-sans break-words whitespace-pre-wrap">
              {p.body_truncated ? `${p.body}…` : p.body}
            </pre>
          )}
        </>
      )}
      <p className="font-mono text-xs break-all text-muted-foreground">{draft.path}</p>
    </article>
  );
}

/**
 * The draft an embedded editor just left with exit 0: its row when the
 * Drafts list shows it, else a row built from a fresh `draft_preview`. A
 * draft that is gone closes the summary, so the reader shows the selection,
 * the message it showed before. `member` is a conversation's draft member
 * (PERSO-146), shown with its draft actions and read again whenever its
 * file's version moves (`draftRevision`, a `draft.changed` naming it), which
 * is how an approve or a back to draft reaches its toolbar; a conversation
 * list reload alone reads nothing (PERSO-272). A member whose file is gone
 * closes with the selection, which the draft's `state.remove` clears.
 */
export function ComposeSummary({ account, draftId, member = false }: { account: string; draftId: string; member?: boolean }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const listed = draftsShown(s) && s.selection.account === account ? filteredDrafts(s.messages.data, "").find((d) => d.id === draftId) : undefined;
  const [read, setRead] = useState<DraftItem | null>(null);
  const known = listed !== undefined;
  const version = member ? draftRevision(s, account, draftId) : "";

  useEffect(() => {
    if (known) return;
    let live = true;
    // StrictMode's remount joins the read on its way; a new version of the file asks again.
    joinRead(`draft_summary:${account}/${draftId}@${version}`, () => cmd.draftPreview(account, draftId)).then(
      (p) => {
        if (!live) return;
        setRead({
          id: p.id,
          selector: p.selector,
          path: p.path,
          status: p.status,
          to: p.to,
          cc: p.cc,
          bcc: p.bcc,
          subject: p.subject,
          date: null,
          // `draft_preview` answered, so the file parses; its `valid` is whether it would send.
          valid: true,
          ready: p.valid,
          diagnostic: null,
        });
      },
      () => live && dispatch({ type: "compose_summary_closed" }),
    );
    return () => {
      live = false;
    };
  }, [account, draftId, known, version, dispatch]);

  const draft = listed ?? read;
  if (!draft) {
    return (
      <div className="flex flex-col gap-2 px-5 py-4" aria-busy="true" aria-label="Loading the draft">
        <Skeleton className="h-6 w-2/3" />
        <Skeleton className="mt-2 h-32 w-full" />
      </div>
    );
  }
  return <DraftPreview account={account} draft={draft} standalone={!member} member={member} />;
}
