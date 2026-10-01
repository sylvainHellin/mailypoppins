// The reader pane for a draft: `draft_preview`'s record (the headers, the
// status and the body the dry run renders), `draft_validate`'s report and
// the `attachments:` list `draft_attachments` reads from the file, read
// again whenever the listing's row changes (a save in the editor).
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
import { useAppState, useDispatch } from "@/app/store";
import { draftsShown, filteredDrafts, targetKey, type DraftItem } from "@/app/state";
import * as cmd from "@/lib/commands";
import { asGuiError, type DraftAttachments } from "@/lib/gui-types";
import type { DraftPreview as Preview, DraftReport } from "@/protocol/types";

type Loaded = {
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
 * on the draft itself, and Close, which brings the selection back.
 */
export function DraftPreview({ account, draft, standalone = false }: { account: string; draft: DraftItem; standalone?: boolean }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const [loaded, setLoaded] = useState<Loaded | null>(null);
  // A row that changed (a save, an approve) is read again.
  const key = `${account}/${draft.id}|${draft.path}|${draft.status}|${draft.subject}|${draft.to}|${draft.valid}|${s.messages.loadedGen}`;
  const invalid = draft.diagnostic !== null || !draft.valid;

  useEffect(() => {
    if (invalid) return;
    let live = true;
    Promise.allSettled([
      cmd.draftPreview(account, draft.id),
      cmd.draftValidate(account, draft.id),
      cmd.draftAttachments(account, draft.id),
    ]).then(([p, v, a]) => {
      if (!live) return;
      setLoaded({
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
  const p = loaded?.key === key ? loaded.preview : null;
  const report = loaded?.key === key ? loaded.report : null;
  const subject = p?.subject || draft.subject || (invalid ? draft.id : "(no subject)");
  const status = draft.status;
  const attachments = loaded?.key === key && loaded.attachments ? draftItemsOf(loaded.attachments) : [];
  const owner = { kind: "draft" as const, account, draftId: draft.id };

  return (
    <article aria-label={`Draft: ${subject}`} data-slot="draft-preview" className="flex flex-col gap-3 px-5 py-4 text-sm">
      <div role="toolbar" aria-label="Draft actions" aria-busy={pending || undefined} className="flex flex-wrap items-center gap-1">
        <Button
          size="sm"
          variant="ghost"
          title={standalone ? "Edit in the editor" : "Edit in the editor (e)"}
          onClick={() => (standalone ? void openInEditor(dispatch, account, draft.id, draft.path) : runAction("open_editor", s, dispatch))}
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
            <Button size="sm" variant="ghost" title="Attach file (ta)" onClick={() => runAction("attach_file", s, dispatch)}>
              <Paperclip aria-hidden="true" />
              Attach file
            </Button>
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
      ) : loaded?.key === key && loaded.error ? (
        <p data-slot="draft-diagnostic" className="text-destructive">
          The preview did not load: {loaded.error}
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
                        (list) => list && setLoaded((l) => (l && l.key === key ? { ...l, attachments: list } : l)),
                      )
                    }
                  >
                    <X aria-hidden="true" />
                  </Button>
                </li>
              ))}
            </ul>
          ) : null}
          <pre data-slot="draft-body" className="font-sans break-words whitespace-pre-wrap">
            {p.body_truncated ? `${p.body}…` : p.body}
          </pre>
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
 * the message it showed before.
 */
export function ComposeSummary({ account, draftId }: { account: string; draftId: string }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const listed = draftsShown(s) && s.selection.account === account ? filteredDrafts(s.messages.data, "").find((d) => d.id === draftId) : undefined;
  const [read, setRead] = useState<DraftItem | null>(null);
  const known = listed !== undefined;

  useEffect(() => {
    if (known) return;
    let live = true;
    cmd.draftPreview(account, draftId).then(
      (p) => {
        if (!live) return;
        setRead({
          id: p.id,
          selector: p.selector,
          path: p.path,
          status: p.status,
          to: p.to,
          cc: p.cc,
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
  }, [account, draftId, known, dispatch]);

  const draft = listed ?? read;
  if (!draft) {
    return (
      <div className="flex flex-col gap-2 px-5 py-4" aria-busy="true" aria-label="Loading the draft">
        <Skeleton className="h-6 w-2/3" />
        <Skeleton className="mt-2 h-32 w-full" />
      </div>
    );
  }
  return <DraftPreview account={account} draft={draft} standalone />;
}
