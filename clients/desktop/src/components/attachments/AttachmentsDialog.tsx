// The attachment dialogs (clients/desktop/docs/reader.md, "Attachments"):
// the TUI's `to` picker over several attachments, the Save dialog `ts` opens
// with a directory field where the TUI has its directory picker, and the
// path field `ta` opens. The two fields stand in for the native file and
// folder pickers, which arrive with the dialog plugin.

import { useEffect, useId, useRef, useState } from "react";
import { FileWarning, Paperclip } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { formatSize } from "@/components/list/format";
import { attachPath, openItem, saveParts } from "@/app/attachments";
import { useAppState, useDispatch } from "@/app/store";
import type { AttachmentDialog as Dialogs, AttachmentItem, AttachmentOwner } from "@/app/state";

export const PICKER_NOTE = "A native picker arrives with the dialog plugin; until then, type the path.";

function Size({ item }: { item: AttachmentItem }) {
  if (item.missing) return <span className="text-destructive">missing</span>;
  return item.size === null ? null : <span className="text-muted-foreground tabular-nums">{formatSize(item.size)}</span>;
}

/** The `to` picker: one button per attachment; the dialog closes on an open. */
function OpenList({
  owner,
  items,
  onDone,
  firstRef,
}: {
  owner: AttachmentOwner;
  items: AttachmentItem[];
  onDone: () => void;
  firstRef: React.RefObject<HTMLButtonElement | null>;
}) {
  const dispatch = useDispatch();
  const first = items.find((i) => !i.missing)?.part;
  return (
    <ul aria-label="Attachments" className="flex flex-col gap-1">
      {items.map((item) => (
        <li key={item.part}>
          <Button
            type="button"
            variant="ghost"
            ref={item.part === first ? firstRef : undefined}
            data-attachment={item.part}
            disabled={item.missing}
            className="w-full justify-start gap-2"
            aria-label={`Open ${item.name}${item.missing ? " (missing)" : ""}`}
            onClick={() => {
              onDone();
              void openItem(owner, item, dispatch);
            }}
          >
            {item.missing ? <FileWarning aria-hidden="true" /> : <Paperclip aria-hidden="true" />}
            <span className="min-w-0 flex-1 truncate text-left">{item.name}</span>
            <Size item={item} />
          </Button>
        </li>
      ))}
    </ul>
  );
}

/** The Save dialog: which parts, and the directory, the last one used offered first. */
function SaveForm({ dialog, inputRef }: { dialog: Extract<Dialogs, { kind: "save" }>; inputRef: React.RefObject<HTMLInputElement | null> }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const id = useId();
  const [dir, setDir] = useState(s.saveDir);
  const [checked, setChecked] = useState<number[]>(() => dialog.items.map((i) => i.part));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    if (busy) return;
    setBusy(true);
    const parts = dialog.items.map((i) => i.part).filter((p) => checked.includes(p));
    const refusal = await saveParts(dialog.account, dialog.row_id, parts, dir, dispatch);
    setBusy(false);
    setError(refusal);
  };

  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      {dialog.items.length > 1 ? (
        <fieldset className="flex flex-col gap-1">
          <legend className="mb-1 text-sm text-muted-foreground">Attachments</legend>
          {dialog.items.map((item) => (
            <label key={item.part} className="flex items-center gap-2 text-sm">
              <input
                type="checkbox"
                checked={checked.includes(item.part)}
                onChange={(e) => {
                  const on = e.currentTarget.checked;
                  setChecked((c) => (on ? [...c, item.part] : c.filter((p) => p !== item.part)));
                }}
              />
              <span className="min-w-0 flex-1 truncate">{item.name}</span>
              <Size item={item} />
            </label>
          ))}
        </fieldset>
      ) : (
        <p className="flex items-center gap-2 text-sm">
          <Paperclip aria-hidden="true" className="size-4 text-muted-foreground" />
          <span className="min-w-0 flex-1 truncate">{dialog.items[0]?.name}</span>
          {dialog.items[0] ? <Size item={dialog.items[0]} /> : null}
        </p>
      )}
      <div className="flex flex-col gap-1">
        <label htmlFor={`${id}-dir`} className="text-sm text-muted-foreground">
          Directory
        </label>
        <Input
          id={`${id}-dir`}
          ref={inputRef}
          value={dir}
          autoComplete="off"
          spellCheck={false}
          aria-describedby={`${id}-note`}
          aria-invalid={error !== null ? true : undefined}
          onChange={(e) => setDir(e.currentTarget.value)}
        />
        <p id={`${id}-note`} className="text-xs text-muted-foreground">
          An absolute path, or one starting with ~. A name already there gets _1. {PICKER_NOTE}
        </p>
      </div>
      <p role="alert" data-slot="attachments-error" className="min-h-5 text-sm text-destructive">
        {error}
      </p>
      <DialogFooter>
        <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
        <Button type="submit" disabled={busy || checked.length === 0}>
          Save
        </Button>
      </DialogFooter>
    </form>
  );
}

/** The `ta` dialog: a path field until the native file picker is installed. */
function AttachForm({ dialog, inputRef }: { dialog: Extract<Dialogs, { kind: "attach" }>; inputRef: React.RefObject<HTMLInputElement | null> }) {
  const dispatch = useDispatch();
  const id = useId();
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = async () => {
    if (busy) return;
    setBusy(true);
    const refusal = await attachPath(dialog.account, dialog.draftId, path, dispatch);
    setBusy(false);
    setError(refusal);
  };

  return (
    <form
      className="flex flex-col gap-3"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <div className="flex flex-col gap-1">
        <label htmlFor={`${id}-path`} className="text-sm text-muted-foreground">
          File
        </label>
        <Input
          id={`${id}-path`}
          ref={inputRef}
          value={path}
          placeholder="~/Documents/report.pdf"
          autoComplete="off"
          spellCheck={false}
          aria-describedby={`${id}-note`}
          aria-invalid={error !== null ? true : undefined}
          onChange={(e) => setPath(e.currentTarget.value)}
        />
        <p id={`${id}-note`} className="text-xs text-muted-foreground">
          An absolute path, or one starting with ~, kept as typed in the draft&apos;s attachments. {PICKER_NOTE}
        </p>
      </div>
      <p role="alert" data-slot="attachments-error" className="min-h-5 text-sm text-destructive">
        {error}
      </p>
      <DialogFooter>
        <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
        <Button type="submit" disabled={busy}>
          Attach
        </Button>
      </DialogFooter>
    </form>
  );
}

const TITLE: Record<Dialogs["kind"], string> = {
  open: "Open attachment",
  save: "Save attachments",
  attach: "Attach file",
};

export type AttachmentsDialogProps = { dialog: Dialogs | null; onOpenChange: (open: boolean) => void };

export function AttachmentsDialog({ dialog, onOpenChange }: AttachmentsDialogProps) {
  const fieldRef = useRef<HTMLInputElement>(null);
  const firstRef = useRef<HTMLButtonElement>(null);
  const [shown, setShown] = useState(dialog);
  // Keep the last dialog while the popup animates out.
  useEffect(() => {
    if (dialog) setShown(dialog);
  }, [dialog]);
  const d = dialog ?? shown;
  const kind = d?.kind ?? "open";
  const title = d?.kind === "save" && d.items.length === 1 ? "Save attachment" : TITLE[kind];
  const description = d ? (d.kind === "attach" ? `To the draft ${d.subject}` : d.subject) : "";
  return (
    <Dialog open={dialog !== null} onOpenChange={onOpenChange}>
      <DialogContent
        initialFocus={kind === "open" ? firstRef : fieldRef}
        data-slot="attachments-dialog"
        data-kind={kind}
        className="sm:max-w-md"
      >
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        {d?.kind === "open" ? (
          <OpenList owner={d.owner} items={d.items} onDone={() => onOpenChange(false)} firstRef={firstRef} />
        ) : d?.kind === "save" ? (
          <SaveForm key={`${d.account}/${d.row_id}/${d.items.map((i) => i.part).join(",")}`} dialog={d} inputRef={fieldRef} />
        ) : d?.kind === "attach" ? (
          <AttachForm key={`${d.account}/${d.draftId}`} dialog={d} inputRef={fieldRef} />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
