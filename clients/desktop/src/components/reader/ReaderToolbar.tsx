import { Archive, ChevronDown, Copy, FolderInput, Forward, Globe, Mail, MailOpen, Reply, ReplyAll, Star, StarOff, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import { copyFromMessage } from "@/app/interop";
import { openMessageTarget, runMutation, type MutationActionId } from "@/app/actions";
import * as compose from "@/app/compose";
import { openHtml } from "@/app/attachments";
import { READER_MODE_LABELS, READER_MODES, saveReaderMode } from "@/app/readerMode";
import { useAppState, useDispatch } from "@/app/store";
import { targetKey } from "@/app/state";
import type { MessageMeta } from "@/lib/gui-types";

/**
 * The open message's actions, the same the keys run: Reply and Reply all
 * open the editor on the new draft, Forward asks for the recipients first,
 * Archive and Delete ask first, Move opens the picker. They act on this
 * message only, whatever the list has marked. Open in browser hands the
 * daemon's rendition to the default browser. Copy is a menu of the sender's
 * address, the `mp://` link (what `y` copies) and the subject. HTML | Text
 * is the reader mode (`tt`), the current one pressed.
 */
export function ReaderToolbar({ meta }: { meta: MessageMeta }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const target = openMessageTarget(s);
  const read = meta.flags.includes("read");
  const flagged = meta.flags.includes("flagged");
  const run = (id: MutationActionId) => target && runMutation(id, [target], s, dispatch, false);
  const busy = target !== null && targetKey(target) in s.pending;
  const src = target ? compose.rowSource(s, target) : null;
  return (
    <div
      role="toolbar"
      aria-label="Message actions"
      aria-busy={busy || undefined}
      className="flex flex-wrap items-center gap-1 border-b border-border px-4 py-1.5"
    >
      <Button size="sm" variant="ghost" title="Reply (r)" onClick={() => compose.reply(src, false, dispatch)}>
        <Reply aria-hidden="true" />
        Reply
      </Button>
      <Button size="sm" variant="ghost" title="Reply all (ca)" onClick={() => compose.reply(src, true, dispatch)}>
        <ReplyAll aria-hidden="true" />
        Reply all
      </Button>
      <Button size="sm" variant="ghost" title="Forward (cf)" onClick={() => compose.forward(src, dispatch)}>
        <Forward aria-hidden="true" />
        Forward
      </Button>
      <Button size="sm" variant="ghost" title="Archive (a)" onClick={() => run("archive")}>
        <Archive aria-hidden="true" />
        Archive
      </Button>
      <Button size="sm" variant="ghost" title="Delete (d)" onClick={() => run("delete")}>
        <Trash2 aria-hidden="true" />
        Delete
      </Button>
      <Button size="sm" variant="ghost" title="Move to mailbox (M)" onClick={() => run("move")}>
        <FolderInput aria-hidden="true" />
        Move
      </Button>
      <Button size="sm" variant="ghost" title="Toggle flag (*)" onClick={() => run("toggle_flag")}>
        {flagged ? <StarOff aria-hidden="true" /> : <Star aria-hidden="true" />}
        {flagged ? "Unflag" : "Flag"}
      </Button>
      <Button size="sm" variant="ghost" title="Toggle read (u)" onClick={() => run("toggle_read")}>
        {read ? <Mail aria-hidden="true" /> : <MailOpen aria-hidden="true" />}
        {read ? "Mark unread" : "Mark read"}
      </Button>
      <Button size="sm" variant="ghost" title="Open HTML in browser (t b)" onClick={() => void openHtml(s, dispatch)}>
        <Globe aria-hidden="true" />
        Open in browser
      </Button>
      <div role="group" aria-label="Reader mode" className="flex">
        {READER_MODES.map((mode, i) => (
          <Button
            key={mode}
            size="sm"
            variant="outline"
            title={`${mode === "html" ? "Show the HTML version" : "Show the plain text"} (t t)`}
            aria-pressed={s.readerMode === mode}
            className={`aria-pressed:border-framing aria-pressed:bg-selection aria-pressed:text-selection-foreground ${i === 0 ? "rounded-r-none" : "-ml-px rounded-l-none"}`}
            onClick={() => void saveReaderMode(dispatch, mode)}
          >
            {READER_MODE_LABELS[mode]}
          </Button>
        ))}
      </div>
      <DropdownMenu>
        <DropdownMenuTrigger render={<Button size="sm" variant="ghost" title="Copy from this message" />}>
          <Copy aria-hidden="true" />
          Copy
          <ChevronDown aria-hidden="true" />
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="w-auto">
          <DropdownMenuItem onClick={() => copyFromMessage(meta, "sender", dispatch)}>Copy sender address</DropdownMenuItem>
          <DropdownMenuItem onClick={() => copyFromMessage(meta, "link", dispatch)}>Copy link (mp://)</DropdownMenuItem>
          <DropdownMenuItem onClick={() => copyFromMessage(meta, "subject", dispatch)}>Copy subject</DropdownMenuItem>
        </DropdownMenuContent>
      </DropdownMenu>
    </div>
  );
}
