import { useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ConfirmMutationDialog, type Confirmation } from "@/components/mutations/ConfirmMutationDialog";
import {
  createSignature,
  cursorAfterRemoval,
  deleteSignature,
  deleteTitle,
  editSignature,
  keptCursor,
  renameSignature,
  toggleDefault,
} from "@/app/signatures";
import type { SignaturesDialog as SignaturesDialogState } from "@/app/state";
import { useAppState, useDispatch } from "@/app/store";
import { isEditable } from "@/keymap/useKeymap";
import * as cmd from "@/lib/commands";
import { asGuiError, type SignatureFile } from "@/lib/gui-types";

export type SignaturesDialogProps = { dialog: SignaturesDialogState | null; onOpenChange: (open: boolean) => void };

/** Browsing the list, or which name field owns the keyboard. */
type Mode = "list" | "new" | "rename";

/** The selected signature's body, or why it could not be read. */
type Preview = { name: string; file: SignatureFile | null; error: string | null };

/**
 * The Signatures dialog (`cs`), the TUI's signatures overlay: every
 * signature with the account's default marked and the selected one's body
 * beside the list. `j`/Down and `k`/Up move, Enter makes the selected
 * signature the account's default or clears it when it already is, `e`
 * opens it in the editor, `n` asks for a name and creates it (then opens it
 * in the editor), `r` asks for a new name seeded with the old one, `d`
 * deletes it behind a confirmation, Escape or `q` closes. In a name field
 * Enter commits and Escape goes back to the list. The dialog owns every key
 * while it is open.
 */
export function SignaturesDialog({ dialog, onOpenChange }: SignaturesDialogProps) {
  const dispatch = useDispatch();
  const state = useAppState();
  const id = useId();
  const listRef = useRef<HTMLDivElement>(null);
  const nameRef = useRef<HTMLInputElement>(null);
  const open = dialog !== null;
  const account = dialog?.account ?? "";
  const loadable = dialog ? state.signatures[dialog.account] : undefined;
  const listing = loadable?.data ?? null;
  const names = listing?.names ?? [];
  const fallback = listing?.default ?? null;

  const [cursor, setCursor] = useState<string | null>(null);
  const [mode, setMode] = useState<Mode>("list");
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [confirm, setConfirm] = useState<(Confirmation & { name: string }) | null>(null);
  const [busy, setBusy] = useState(false);

  // A new dialog starts on its first row, in the list.
  useEffect(() => {
    setCursor(null);
    setMode("list");
    setName("");
    setError(null);
    setPreview(null);
    setConfirm(null);
    setBusy(false);
  }, [dialog]);

  // The cursor stays on its name while the listing changes, else the first row.
  useEffect(() => {
    if (listing) setCursor((c) => keptCursor(listing.names, c));
  }, [listing]);

  // The selected body, read again whenever the listing is (a save in the
  // editor comes back as `signature.changed`, which reads the listing again).
  useEffect(() => {
    if (!open || !cursor || !listing) {
      setPreview(null);
      return;
    }
    let live = true;
    const shown = cursor;
    cmd
      .signatureRead(shown)
      .then((file) => live && setPreview({ name: shown, file, error: null }))
      .catch((e: unknown) => live && setPreview({ name: shown, file: null, error: asGuiError(e).message }));
    return () => {
      live = false;
    };
  }, [open, cursor, listing]);

  const toList = () => {
    setMode("list");
    setName("");
    listRef.current?.focus();
  };

  const latest = useRef({ cursor, names, fallback, mode, confirm, busy, account, preview });
  latest.current = { cursor, names, fallback, mode, confirm, busy, account, preview };

  const actions = {
    move(by: 1 | -1) {
      const { names: ns, cursor: c } = latest.current;
      if (ns.length === 0) return;
      const at = c === null ? -1 : ns.indexOf(c);
      setCursor(ns[Math.max(0, Math.min(ns.length - 1, at + by))]);
    },
    async toggle() {
      const { cursor: c, fallback: d, account: a } = latest.current;
      if (!c) return setError("No signature selected");
      setBusy(true);
      const out = await toggleDefault(dispatch, a, c, c === d);
      setBusy(false);
      setError(out.ok ? null : out.error);
    },
    async edit() {
      const { cursor: c, preview: p } = latest.current;
      if (!c) return setError("No signature to edit; press n to create one");
      let path = p?.name === c ? p.file?.path : undefined;
      if (!path) {
        try {
          path = (await cmd.signatureRead(c)).path;
        } catch (e: unknown) {
          return setError(`Cannot open signature: ${asGuiError(e).message}`);
        }
      }
      setError(await editSignature(dispatch, path, c));
    },
    startNew() {
      setError(null);
      setName("");
      setMode("new");
    },
    startRename() {
      const { cursor: c } = latest.current;
      if (!c) return setError("No signature to rename");
      setError(null);
      setName(c);
      setMode("rename");
    },
    async askDelete() {
      const { cursor: c, preview: p } = latest.current;
      if (!c) return setError("No signature to delete");
      let path = p?.name === c ? p.file?.path : undefined;
      if (!path) {
        try {
          path = (await cmd.signatureRead(c)).path;
        } catch (e: unknown) {
          return setError(`Cannot delete: ${asGuiError(e).message}`);
        }
      }
      setError(null);
      setConfirm({ kind: "signature_delete", name: c, title: deleteTitle(c), detail: path });
    },
  };
  const actionsRef = useRef(actions);
  actionsRef.current = actions;

  const confirmDelete = async () => {
    const pending = confirm;
    if (!pending) return;
    setConfirm(null);
    setBusy(true);
    const out = await deleteSignature(dispatch, account, pending.name);
    setBusy(false);
    setError(out.ok ? null : out.error);
    if (out.ok) setCursor(cursorAfterRemoval(names, pending.name));
    listRef.current?.focus();
  };

  const commitName = async (e?: FormEvent) => {
    e?.preventDefault();
    if (busy) return;
    const typed = name;
    setBusy(true);
    if (mode === "new") {
      const out = await createSignature(dispatch, typed);
      setBusy(false);
      if (!out.ok) return setError(out.error);
      setError(null);
      setCursor(out.value.name);
      toList();
      // What `n` is for: the new file opens in the editor.
      setError(await editSignature(dispatch, out.value.path, out.value.name));
      return;
    }
    const old = cursor;
    if (!old) {
      setBusy(false);
      return setError("No signature to rename");
    }
    const out = await renameSignature(dispatch, account, old, typed);
    setBusy(false);
    if (!out.ok) return setError(out.error);
    setError(null);
    setCursor(typed);
    toList();
  };

  // The name field takes the typing; the field's Escape goes back to the list.
  useEffect(() => {
    if (mode !== "list") nameRef.current?.focus();
  }, [mode]);

  // On the window, so the keys work before the popup has taken focus, and in
  // the capture phase, since the popup stops the arrow keys on their way up.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      const { mode: m, confirm: c, busy: b } = latest.current;
      // The confirmation over the list, and a name field, own their keys.
      if (m !== "list" || c || isEditable(e.target)) return;
      const run = actionsRef.current;
      switch (e.key) {
        case "j":
        case "ArrowDown":
          e.preventDefault();
          return run.move(1);
        case "k":
        case "ArrowUp":
          e.preventDefault();
          return run.move(-1);
        case "q":
          e.preventDefault();
          return onOpenChange(false);
        case "Enter": {
          // A button of the dialog that has the focus takes its own Enter.
          const active = document.activeElement;
          const popup = listRef.current?.closest('[role="dialog"]');
          if (active instanceof HTMLButtonElement && popup?.contains(active)) return;
          e.preventDefault();
          if (!b) void run.toggle();
          return;
        }
        default:
          break;
      }
      if (b) return;
      if (e.key === "e") return e.preventDefault(), void run.edit();
      if (e.key === "n") return e.preventDefault(), run.startNew();
      if (e.key === "r") return e.preventDefault(), run.startRename();
      if (e.key === "d") return e.preventDefault(), void run.askDelete();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open, onOpenChange]);

  // Escape in a name field goes back to the list, and over the confirmation
  // it closes only the confirmation.
  const onDialogOpenChange = (next: boolean) => {
    if (next) return onOpenChange(true);
    if (confirm) return setConfirm(null);
    if (mode !== "list") return toList();
    onOpenChange(false);
  };

  const onNameKey = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    toList();
  };

  const optionId = (n: string) => `${id}-option-${names.indexOf(n)}`;
  const selectedIsDefault = cursor !== null && cursor === fallback;
  const empty = listing !== null && names.length === 0;
  const body =
    preview && preview.name === cursor ? (preview.error ?? (preview.file?.content === "" ? "(empty)" : (preview.file?.content ?? ""))) : "";

  return (
    <Dialog open={open} onOpenChange={onDialogOpenChange}>
      <DialogContent initialFocus={listRef} data-slot="signatures-dialog" className="sm:max-w-2xl">
        <DialogHeader>
          <DialogTitle>Signatures</DialogTitle>
          <DialogDescription>
            Signatures are shared by every account; the default is {account}&apos;s.
          </DialogDescription>
        </DialogHeader>
        <div className="grid min-h-40 grid-cols-[minmax(9rem,13rem)_1fr] gap-3">
          <div
            ref={listRef}
            role="listbox"
            aria-label="Signatures"
            tabIndex={0}
            aria-activedescendant={cursor !== null && names.includes(cursor) ? optionId(cursor) : undefined}
            className="flex max-h-72 flex-col gap-0.5 overflow-y-auto rounded-lg outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
          >
            {listing === null && loadable?.error ? (
              <p className="px-2 py-1.5 text-sm text-destructive">{loadable.error.message}</p>
            ) : null}
            {empty ? <p className="px-2 py-1.5 text-sm text-muted-foreground">No signatures yet; press n to create one</p> : null}
            {names.map((n) => (
              <div
                key={n}
                id={optionId(n)}
                role="option"
                aria-selected={n === cursor}
                data-signature={n}
                onClick={() => setCursor(n)}
                onDoubleClick={() => {
                  setCursor(n);
                  void actions.edit();
                }}
                className="flex cursor-default items-center justify-between gap-2 rounded-md px-2 py-1.5 text-sm aria-selected:bg-accent aria-selected:text-accent-foreground"
              >
                <span className="truncate">{n}</span>
                {n === fallback ? (
                  <Badge data-slot="signature-default" variant="secondary">
                    default
                  </Badge>
                ) : null}
              </div>
            ))}
          </div>
          <section aria-label="Preview" className="min-w-0 rounded-lg border border-border p-2">
            <pre data-slot="signature-preview" className="max-h-72 overflow-auto font-mono text-xs whitespace-pre-wrap">
              {body}
            </pre>
          </section>
        </div>
        {mode !== "list" ? (
          <form className="grid grid-cols-[auto_1fr] items-center gap-2" onSubmit={(e) => void commitName(e)}>
            <label htmlFor={`${id}-name`} className="text-sm text-muted-foreground">
              {mode === "new" ? "New signature" : `Rename '${cursor ?? ""}' to`}
            </label>
            <Input
              id={`${id}-name`}
              ref={nameRef}
              data-slot="signature-name"
              value={name}
              autoComplete="off"
              spellCheck={false}
              aria-invalid={error !== null ? true : undefined}
              onChange={(e) => setName(e.currentTarget.value)}
              onKeyDown={onNameKey}
            />
          </form>
        ) : null}
        <p role="alert" data-slot="signatures-error" className="min-h-5 text-sm text-destructive">
          {error}
        </p>
        <DialogFooter className="flex-wrap">
          <Button variant="outline" disabled={busy} onClick={actions.startNew}>
            New <Kbd aria-hidden="true">n</Kbd>
          </Button>
          <Button variant="outline" disabled={busy || cursor === null} onClick={actions.startRename}>
            Rename <Kbd aria-hidden="true">r</Kbd>
          </Button>
          <Button variant="outline" disabled={busy || cursor === null} onClick={() => void actions.edit()}>
            Edit <Kbd aria-hidden="true">e</Kbd>
          </Button>
          <Button variant="outline" disabled={busy || cursor === null} onClick={() => void actions.askDelete()}>
            Delete <Kbd aria-hidden="true">d</Kbd>
          </Button>
          <Button disabled={busy || cursor === null} onClick={() => void actions.toggle()}>
            {selectedIsDefault ? "Clear default" : "Set default"} <Kbd aria-hidden="true">↵</Kbd>
          </Button>
        </DialogFooter>
        <ConfirmMutationDialog
          dialog={confirm}
          onOpenChange={(o) => {
            if (!o) {
              setConfirm(null);
              listRef.current?.focus();
            }
          }}
          onConfirm={() => void confirmDelete()}
        />
      </DialogContent>
    </Dialog>
  );
}
