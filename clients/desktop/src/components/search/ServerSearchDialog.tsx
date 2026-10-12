// The Server Search form (`ff`, PERSO-115), the TUI overlay's form (#0086b):
// Search In (Current Mailbox / Current Account), From, To, Subject, Keywords,
// After, Before, the attachment toggle, and the raw-grammar Advanced line,
// which greys the structured fields once it holds anything. The results land
// in the list pane, where every search's do, so a hit is a list row there.

import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { advancedActive, SCOPE_LABEL, submitServerSearch } from "@/app/search";
import { useDispatch } from "@/app/store";
import type { SearchFormState, ServerSearchDialog as Dialogue } from "@/app/state";
import type { SearchForm } from "@/lib/gui-types";

type TextField = Exclude<keyof SearchForm, "attachment">;

export type ServerSearchDialogProps = { dialog: Dialogue | null; onOpenChange: (open: boolean) => void };

/**
 * Tab and Shift+Tab move through the fields, Space flips the scope and the
 * attachment toggles, Enter searches from anywhere in the form, and Escape
 * closes it, the TUI overlay's keys. What the parser refuses shows in the
 * form's alert, which keeps what was typed.
 */
export function ServerSearchDialog({ dialog, onOpenChange }: ServerSearchDialogProps) {
  const dispatch = useDispatch();
  const id = useId();
  const [form, setForm] = useState<SearchFormState | null>(dialog?.form ?? null);
  const [problem, setProblem] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const fromRef = useRef<HTMLInputElement>(null);
  const advancedRef = useRef<HTMLInputElement>(null);
  const open = dialog !== null;

  useEffect(() => {
    setForm(dialog?.form ?? null);
    setProblem(null);
    setBusy(false);
  }, [dialog]);

  const fields = form?.fields;
  const greyed = fields ? advancedActive(fields) : false;

  const submit = async () => {
    if (!dialog || !form || busy) return;
    setBusy(true);
    const refused = await submitServerSearch(dispatch, dialog, form);
    setBusy(false);
    setProblem(refused);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLFormElement>) => {
    if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
    // Enter on Cancel or the dialog's close button is theirs.
    if (e.target instanceof HTMLElement && e.target.closest("[data-own-enter]")) return;
    e.preventDefault();
    void submit();
  };

  const setText = (name: TextField) => (e: { currentTarget: { value: string } }) => {
    if (!form) return;
    setForm({ ...form, fields: { ...form.fields, [name]: e.currentTarget.value } });
    setProblem(null);
  };

  // `htmlFor` null: the control carries its own label (the attachment toggle).
  const row = (label: string, htmlFor: string | null, control: ReactNode, greyable = true) => {
    const tone = `text-sm ${greyable && greyed ? "text-muted-foreground/60" : "text-muted-foreground"}`;
    return (
      <div className="grid grid-cols-[6rem_1fr] items-center gap-2">
        {htmlFor ? (
          <label htmlFor={htmlFor} className={tone}>
            {label}
          </label>
        ) : (
          <span aria-hidden="true" className={tone}>
            {label}
          </span>
        )}
        {control}
      </div>
    );
  };
  const text = (name: TextField, label: string, placeholder?: string) =>
    row(
      label,
      `${id}-${name}`,
      <Input
        id={`${id}-${name}`}
        ref={name === "from" ? fromRef : name === "advanced" ? advancedRef : undefined}
        data-field={name}
        value={fields?.[name] ?? ""}
        placeholder={placeholder}
        autoComplete="off"
        spellCheck={false}
        // A non-blank Advanced line takes over, as in the TUI: the
        // structured fields are greyed, and Tab skips them.
        disabled={name !== "advanced" && greyed}
        onChange={setText(name)}
      />,
      name !== "advanced",
    );

  const scope = form?.scope ?? "account";
  const where =
    scope === "mailbox"
      ? dialog?.mailbox
        ? `the mailbox ${dialog.mailbox} of ${dialog.account}`
        : `the selected mailbox of ${dialog?.account ?? ""}`
      : `every mailbox of ${dialog?.account ?? ""}`;

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={greyed ? advancedRef : fromRef} data-slot="server-search" className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>Server Search</DialogTitle>
          <DialogDescription>{dialog ? `Searches ${where}; the store answers first, then the server.` : ""}</DialogDescription>
        </DialogHeader>
        <form
          aria-label="Server Search"
          className="flex flex-col gap-2"
          onKeyDown={onKeyDown}
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          {row(
            "Search In",
            `${id}-scope`,
            <Button
              id={`${id}-scope`}
              type="button"
              variant="outline"
              size="sm"
              className="justify-start"
              data-field="scope"
              aria-label={`Search in: ${SCOPE_LABEL[scope]}`}
              title="Space toggles Current Mailbox and Current Account"
              // Space toggles (the button's click); Enter searches, as on
              // every field, since the form's handler takes it first.
              onClick={() => {
                if (!form) return;
                setForm({ ...form, scope: scope === "account" ? "mailbox" : "account" });
                setProblem(null);
              }}
            >
              {SCOPE_LABEL[scope]}
            </Button>,
            false,
          )}
          {text("from", "From")}
          {text("to", "To")}
          {text("subject", "Subject")}
          {text("keywords", "Keywords", "words, \"a phrase\", a OR b")}
          {text("after", "After", "YYYY-MM-DD")}
          {text("before", "Before", "YYYY-MM-DD")}
          {row(
            "Attachment",
            null,
            <label className={`flex items-center gap-2 text-sm ${greyed ? "text-muted-foreground/60" : ""}`}>
              <input
                type="checkbox"
                data-field="attachment"
                checked={fields?.attachment ?? false}
                disabled={greyed}
                onChange={(e) => {
                  if (!form) return;
                  setForm({ ...form, fields: { ...form.fields, attachment: e.currentTarget.checked } });
                  setProblem(null);
                }}
              />
              has attachment
            </label>,
          )}
          {text("advanced", "Advanced", "from:x (a OR b) has:attachment …")}
          <p className="text-xs text-muted-foreground">
            <Kbd>Tab</Kbd> fields · <Kbd>Space</Kbd> toggle · <Kbd>Enter</Kbd> search · <Kbd>Esc</Kbd> close
          </p>
          <p
            role="alert"
            data-slot="server-search-problem"
            className="min-h-5 font-mono text-xs break-words whitespace-pre-wrap text-destructive"
          >
            {problem}
          </p>
          <DialogFooter>
            <DialogClose render={<Button type="button" variant="outline" data-own-enter="" />}>Cancel</DialogClose>
            <Button type="submit" disabled={busy || !form}>
              Search <Kbd aria-hidden="true">↵</Kbd>
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
