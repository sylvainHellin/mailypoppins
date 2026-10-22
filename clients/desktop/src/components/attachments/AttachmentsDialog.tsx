// The attachment dialogs (clients/desktop/docs/reader.md, "Attachments"):
// the TUI's `to` picker over several attachments, the Save dialog `ts` opens
// with a directory field where the TUI has its directory picker, the Export
// dialog `te` opens with the same field, and the path field `ta` opens. Each field has a Browse button beside it, the native
// folder or file picker (tauri-plugin-dialog), which fills the field; the
// typed path stays, for the keyboard and wherever no picker opens.

import { useEffect, useId, useRef, useState } from "react";
import { FileWarning, FolderOpen, Mail, Paperclip } from "lucide-react";
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
import { attachPath, exportTo, openItem, pickPath, saveParts } from "@/app/attachments";
import { useAppState, useDispatch } from "@/app/store";
import type { AttachmentDialog as Dialogs, AttachmentItem, AttachmentOwner } from "@/app/state";
import type { MessageSource } from "@/lib/gui-types";

export const PICKER_NOTE = "Browse opens the system picker.";

/**
 * Browse: the native picker, which fills the field and hands the focus to
 * the dialog's submit button, so Enter or a click then saves or attaches.
 * Closing the picker changes nothing; a picker that does not open says so
 * in the dialog's alert and leaves the field to be typed.
 */
function BrowseButton({
  kind,
  current,
  label,
  pickerTitle,
  onPicked,
  onError,
  submitRef,
}: {
  kind: "directory" | "file";
  current: string;
  label: string;
  pickerTitle?: string;
  onPicked: (path: string) => void;
  onError: (message: string) => void;
  submitRef: React.RefObject<HTMLButtonElement | null>;
}) {
  const [picking, setPicking] = useState(false);
  const browse = async () => {
    if (picking) return;
    setPicking(true);
    try {
      const picked = await pickPath(kind, current, pickerTitle);
      if (picked !== null) {
        onPicked(picked);
        submitRef.current?.focus();
      }
    } catch (e: unknown) {
      onError(e instanceof Error ? e.message : String(e));
    } finally {
      setPicking(false);
    }
  };
  return (
    // Busy rather than disabled while the picker is open: a disabled button
    // drops the focus, which a closed picker would then leave nowhere.
    <Button type="button" variant="outline" aria-label={label} aria-busy={picking || undefined} onClick={() => void browse()}>
      <FolderOpen aria-hidden="true" />
      Browse…
    </Button>
  );
}

/** The tag an inline image of the body carries after the real attachments (PERSO-165). */
export function InlineTag({ item }: { item: AttachmentItem }) {
  return item.inline ? (
    <span data-slot="inline-tag" className="shrink-0 text-xs text-muted-foreground group-data-selected/button:text-selection-foreground">
      inline
    </span>
  ) : null;
}

function Size({ item }: { item: AttachmentItem }) {
  if (item.missing) return <span className="text-destructive">missing</span>;
  return item.size === null ? null : (
    <span className="shrink-0 text-muted-foreground tabular-nums group-data-selected/button:text-selection-foreground">
      {formatSize(item.size)}
    </span>
  );
}

/**
 * The `to` picker: one button per attachment; the dialog closes on an open.
 * j/k and the arrows move the cursor and the focus between the parts,
 * skipping a missing one; Enter opens the cursor's part, q closes.
 * The cursor row carries `data-selected="true"`, the value shadcn's
 * `data-selected:` variant matches, painted like the palette's selected
 * item, since `:focus-visible` alone matches only when the webview's
 * heuristic says so and left most rows unmarked.
 */
function OpenList({
  owner,
  items,
  active,
  onDone,
  firstRef,
}: {
  owner: AttachmentOwner;
  items: AttachmentItem[];
  active: boolean;
  onDone: () => void;
  firstRef: React.RefObject<HTMLButtonElement | null>;
}) {
  const dispatch = useDispatch();
  const listRef = useRef<HTMLUListElement>(null);
  const first = items.find((i) => !i.missing)?.part;
  const [cursor, setCursor] = useState(first);
  useEffect(() => {
    if (active) setCursor(first);
  }, [active, first]);

  // On the window, so the keys work before the popup has taken focus, and in
  // the capture phase, since the popup stops the arrow keys on their way up.
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      const down = e.key === "j" || e.key === "ArrowDown";
      const up = e.key === "k" || e.key === "ArrowUp";
      if (e.key === "q") {
        e.preventDefault();
        onDone();
        return;
      }
      if (!down && !up) return;
      e.preventDefault();
      const buttons = Array.from(listRef.current?.querySelectorAll<HTMLButtonElement>("button:not(:disabled)") ?? []);
      if (buttons.length === 0) return;
      const focused = buttons.indexOf(document.activeElement as HTMLButtonElement);
      const at = focused !== -1 ? focused : buttons.findIndex((b) => b.dataset.attachment === String(cursor));
      const next = at === -1 ? 0 : Math.max(0, Math.min(buttons.length - 1, at + (down ? 1 : -1)));
      setCursor(Number(buttons[next].dataset.attachment));
      buttons[next].focus();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [active, onDone, cursor]);

  return (
    <ul ref={listRef} aria-label="Attachments" className="flex min-w-0 flex-col gap-1">
      {items.map((item) => (
        <li key={item.part}>
          <Button
            type="button"
            variant="ghost"
            ref={item.part === first ? firstRef : undefined}
            data-attachment={item.part}
            data-selected={item.part === cursor ? "true" : undefined}
            aria-current={item.part === cursor ? "true" : undefined}
            disabled={item.missing}
            className="w-full justify-start gap-2 focus-visible:border-transparent focus-visible:ring-0 data-selected:bg-selection data-selected:text-selection-foreground data-selected:hover:bg-selection data-selected:hover:text-selection-foreground dark:data-selected:hover:bg-selection data-selected:*:[svg]:text-selection-foreground"
            aria-label={`Open ${item.name}${item.missing ? " (missing)" : ""}`}
            onFocus={() => setCursor(item.part)}
            onClick={() => {
              onDone();
              void openItem(owner, item, dispatch);
            }}
          >
            {item.missing ? <FileWarning aria-hidden="true" /> : <Paperclip aria-hidden="true" />}
            <span className="min-w-0 flex-1 truncate text-left">{item.name}</span>
            <InlineTag item={item} />
            <Size item={item} />
          </Button>
        </li>
      ))}
    </ul>
  );
}

/** The Save dialog: which parts, and the directory, the last one used offered first. */
/** A message source as a React key: its row id, or the hit's Message-ID. */
function sourceKey(source: MessageSource): string {
  return source.kind === "row" ? String(source.row_id) : source.message_id;
}

function SaveForm({ dialog, inputRef }: { dialog: Extract<Dialogs, { kind: "save" }>; inputRef: React.RefObject<HTMLInputElement | null> }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const [dir, setDir] = useState(s.saveDir);
  const [checked, setChecked] = useState<number[]>(() => dialog.items.map((i) => i.part));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submitRef = useRef<HTMLButtonElement>(null);

  const submit = async () => {
    if (busy) return;
    setBusy(true);
    const parts = dialog.items.map((i) => i.part).filter((p) => checked.includes(p));
    const refusal = await saveParts(dialog.account, dialog.source, parts, dir, dispatch);
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
        <fieldset className="flex min-w-0 flex-col gap-1">
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
              <InlineTag item={item} />
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
      <DirectoryField
        dir={dir}
        error={error}
        inputRef={inputRef}
        submitRef={submitRef}
        onDir={setDir}
        onError={setError}
      />
      <p role="alert" data-slot="attachments-error" className="min-h-5 text-sm text-destructive">
        {error}
      </p>
      <DialogFooter>
        <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
        <Button type="submit" ref={submitRef} disabled={busy || checked.length === 0}>
          Save
        </Button>
      </DialogFooter>
    </form>
  );
}

/**
 * The directory field the Save and Export dialogs share: typed, or filled by
 * Browse from the native folder picker, which clears the error.
 */
function DirectoryField({
  dir,
  error,
  inputRef,
  submitRef,
  pickerTitle,
  onDir,
  onError,
}: {
  dir: string;
  error: string | null;
  inputRef: React.RefObject<HTMLInputElement | null>;
  submitRef: React.RefObject<HTMLButtonElement | null>;
  pickerTitle?: string;
  onDir: (dir: string) => void;
  onError: (message: string | null) => void;
}) {
  const id = useId();
  return (
    <div className="flex flex-col gap-1">
      <label htmlFor={`${id}-dir`} className="text-sm text-muted-foreground">
        Directory
      </label>
      <div className="flex gap-2">
        <Input
          id={`${id}-dir`}
          ref={inputRef}
          value={dir}
          autoComplete="off"
          spellCheck={false}
          aria-describedby={`${id}-note`}
          aria-invalid={error !== null ? true : undefined}
          onChange={(e) => onDir(e.currentTarget.value)}
        />
        <BrowseButton
          kind="directory"
          current={dir}
          label="Browse for a directory"
          pickerTitle={pickerTitle}
          submitRef={submitRef}
          onPicked={(path) => {
            onDir(path);
            onError(null);
          }}
          onError={onError}
        />
      </div>
      <p id={`${id}-note`} className="text-xs text-muted-foreground">
        An absolute path, or one starting with ~. A name already there gets _1. {PICKER_NOTE}
      </p>
    </div>
  );
}

/** The `te` dialog: the message as `<date>_<subject>.eml`, into a directory, the last one used offered first. */
function ExportForm({ dialog, inputRef }: { dialog: Extract<Dialogs, { kind: "export" }>; inputRef: React.RefObject<HTMLInputElement | null> }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const [dir, setDir] = useState(s.saveDir);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submitRef = useRef<HTMLButtonElement>(null);

  const submit = async () => {
    if (busy) return;
    setBusy(true);
    const refusal = await exportTo(dialog.account, dialog.source, dir, dispatch);
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
      <p className="flex items-center gap-2 text-sm">
        <Mail aria-hidden="true" className="size-4 text-muted-foreground" />
        <span className="min-w-0 flex-1">The whole message, attachments inside, as an .eml file any mail client opens.</span>
      </p>
      <DirectoryField
        dir={dir}
        error={error}
        inputRef={inputRef}
        submitRef={submitRef}
        pickerTitle="Export message to"
        onDir={setDir}
        onError={setError}
      />
      <p role="alert" data-slot="attachments-error" className="min-h-5 text-sm text-destructive">
        {error}
      </p>
      <DialogFooter>
        <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
        <Button type="submit" ref={submitRef} disabled={busy}>
          Export
        </Button>
      </DialogFooter>
    </form>
  );
}

/** The `ta` dialog: a path field, which Browse fills from the native file picker. */
function AttachForm({ dialog, inputRef }: { dialog: Extract<Dialogs, { kind: "attach" }>; inputRef: React.RefObject<HTMLInputElement | null> }) {
  const dispatch = useDispatch();
  const id = useId();
  const [path, setPath] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submitRef = useRef<HTMLButtonElement>(null);

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
        <div className="flex gap-2">
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
          <BrowseButton
            kind="file"
            current={path}
            label="Browse for a file"
            submitRef={submitRef}
            onPicked={(picked) => {
              setPath(picked);
              setError(null);
            }}
            onError={setError}
          />
        </div>
        <p id={`${id}-note`} className="text-xs text-muted-foreground">
          An absolute path, or one starting with ~, kept as typed in the draft&apos;s attachments. {PICKER_NOTE}
        </p>
      </div>
      <p role="alert" data-slot="attachments-error" className="min-h-5 text-sm text-destructive">
        {error}
      </p>
      <DialogFooter>
        <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
        <Button type="submit" ref={submitRef} disabled={busy}>
          Attach
        </Button>
      </DialogFooter>
    </form>
  );
}

const TITLE: Record<Dialogs["kind"], string> = {
  open: "Open attachment",
  save: "Save attachments",
  export: "Export message",
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
          <OpenList
            owner={d.owner}
            items={d.items}
            active={dialog !== null}
            onDone={() => onOpenChange(false)}
            firstRef={firstRef}
          />
        ) : d?.kind === "save" ? (
          <SaveForm key={`${d.account}/${sourceKey(d.source)}/${d.items.map((i) => i.part).join(",")}`} dialog={d} inputRef={fieldRef} />
        ) : d?.kind === "export" ? (
          <ExportForm
            key={`${d.account}/${sourceKey(d.source)}`}
            dialog={d}
            inputRef={fieldRef}
          />
        ) : d?.kind === "attach" ? (
          <AttachForm key={`${d.account}/${d.draftId}`} dialog={d} inputRef={fieldRef} />
        ) : null}
      </DialogContent>
    </Dialog>
  );
}
