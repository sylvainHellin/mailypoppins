// The event form (CAL-11, PERSO-255; the TUI's `n` overlay, the CLI's
// `mp event create`): Calendar, Title, All day, Start, End, the attendees'
// availability (CAL-14), Location, Attendees, Optional, Rooms, Teams,
// Reminder, Description. It creates
// through `event.create`, and a failed or vanished create opens it again
// with what was typed and a Retry that sends the same transaction id.

import { useEffect, useId, useRef, useState, type ComponentProps, type KeyboardEvent, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Textarea } from "@/components/ui/textarea";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { eventProblem, localZone, submitEvent } from "@/app/eventForm";
import { AvailabilityGrid } from "@/components/calendar/AvailabilityGrid";
import { useDispatch } from "@/app/store";
import type { EventDialog, EventFields } from "@/app/state";

type TextField = "title" | "start" | "end" | "location" | "to" | "optional" | "rooms" | "reminder" | "body";

export type NewEventDialogProps = { dialog: EventDialog | null; onOpenChange: (open: boolean) => void };

/**
 * Enter in a one-line field moves to the next one, Cmd+Enter or Ctrl+Enter
 * creates, Escape cancels. The first problem with the fields as typed, or
 * the daemon's refusal, shows in its alert, and the form stays open with
 * what was typed.
 */
export function NewEventDialog({ dialog, onOpenChange }: NewEventDialogProps) {
  const dispatch = useDispatch();
  const id = useId();
  const calendars = dialog?.calendars ?? [];
  const [fields, setFields] = useState<EventFields | null>(dialog?.fields ?? null);
  const [error, setError] = useState<string | null>(dialog?.error ?? null);
  const [busy, setBusy] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);
  const titleRef = useRef<HTMLInputElement>(null);
  const open = dialog !== null;

  useEffect(() => {
    setFields(dialog?.fields ?? null);
    setError(dialog?.error ?? null);
    setBusy(false);
  }, [dialog]);

  const chosen = calendars.find((c) => c.key === fields?.calendar) ?? null;
  const problem = fields ? eventProblem(fields, calendars) : null;
  const retry = dialog?.transactionId != null;

  const submit = async () => {
    if (!dialog || !fields || busy) return;
    setBusy(true);
    const refused = await submitEvent(dispatch, { ...dialog, fields });
    setBusy(false);
    setError(refused);
  };

  const onKeyDown = (e: KeyboardEvent<HTMLFormElement>) => {
    if (e.key !== "Enter" || e.nativeEvent.isComposing) return;
    if (e.metaKey || e.ctrlKey) {
      e.preventDefault();
      void submit();
      return;
    }
    if (!(e.target instanceof HTMLInputElement) || e.target.type === "checkbox") return;
    e.preventDefault();
    const order = [...(formRef.current?.querySelectorAll<HTMLElement>("[data-field], [data-submit]") ?? [])];
    order[order.indexOf(e.target) + 1]?.focus();
  };

  const change = (next: Partial<EventFields>) => {
    if (!fields) return;
    setFields({ ...fields, ...next });
    setError(null);
  };
  const toggleAllDay = (allDay: boolean) => {
    if (!fields) return;
    const day = fields.start.slice(0, 10);
    change(allDay ? { allDay, start: day, end: "" } : { allDay, start: `${day}T09:00`, end: `${day}T10:00` });
  };

  const row = (name: string, label: string, input: ReactNode) => (
    <div className="grid grid-cols-[6rem_1fr] items-center gap-2">
      <label htmlFor={`${id}-${name}`} className="text-sm text-muted-foreground">
        {label}
      </label>
      {input}
    </div>
  );
  const field = (name: TextField, label: string, extra: ComponentProps<typeof Input> = {}) =>
    row(
      name,
      label,
      <Input
        id={`${id}-${name}`}
        ref={name === "title" ? titleRef : undefined}
        data-field={name}
        value={fields?.[name] ?? ""}
        autoComplete="off"
        spellCheck={name === "title" || name === "location"}
        onChange={(e) => change({ [name]: e.currentTarget.value })}
        {...extra}
      />,
    );
  const check = (name: "allDay" | "teams" | "noReminder", label: string, onChange: (on: boolean) => void, disabled = false) => (
    <label className={`flex items-center gap-2 text-sm ${disabled ? "text-muted-foreground/60" : ""}`}>
      <input
        type="checkbox"
        data-field={name}
        checked={fields?.[name] ?? false}
        disabled={disabled}
        onChange={(e) => onChange(e.currentTarget.checked)}
      />
      {label}
    </label>
  );

  const allDay = fields?.allDay ?? false;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={titleRef} data-slot="new-event" className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{retry ? "New event (retry)" : "New event"}</DialogTitle>
          <DialogDescription>
            {`Times in ${localZone()}; the server sends each attendee and room an invitation.`}
          </DialogDescription>
        </DialogHeader>
        <form
          ref={formRef}
          aria-label="New event"
          className="flex flex-col gap-2"
          onKeyDown={onKeyDown}
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          {row(
            "calendar",
            "Calendar",
            <select
              id={`${id}-calendar`}
              data-field="calendar"
              value={fields?.calendar ?? ""}
              onChange={(e) => {
                const next = calendars.find((c) => c.key === e.currentTarget.value);
                change({ calendar: e.currentTarget.value, teams: next?.teams ? (fields?.teams ?? false) : false });
              }}
              className="h-8 rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
            >
              {calendars.map((c) => (
                <option key={c.key} value={c.key}>
                  {c.label}
                </option>
              ))}
            </select>,
          )}
          {field("title", "Title")}
          <div className="grid grid-cols-[6rem_1fr] items-center gap-2">
            <span />
            {check("allDay", "All day", toggleAllDay)}
          </div>
          {field("start", allDay ? "First day" : "Start", { type: allDay ? "date" : "datetime-local" })}
          {field("end", allDay ? "Last day" : "End", { type: allDay ? "date" : "datetime-local" })}
          <AvailabilityGrid fields={fields} calendar={chosen} />
          {field("location", "Location")}
          {field("to", "Attendees", { placeholder: "ada@example.com, bob@example.com" })}
          {field("optional", "Optional")}
          {field("rooms", "Rooms", { placeholder: "room mailboxes" })}
          <div className="grid grid-cols-[6rem_1fr] items-center gap-2">
            <span />
            <span className="flex flex-col gap-0.5">
              {check("teams", "Teams meeting", (on) => change({ teams: on }), !chosen?.teams)}
              {chosen && !chosen.teams ? (
                <span data-slot="teams-unavailable" className="text-xs text-muted-foreground">
                  This calendar has no online meetings
                </span>
              ) : null}
            </span>
          </div>
          <div className="grid grid-cols-[6rem_1fr] items-center gap-2">
            <label htmlFor={`${id}-reminder`} className="text-sm text-muted-foreground">
              Reminder
            </label>
            <span className="flex items-center gap-3">
              <Input
                id={`${id}-reminder`}
                data-field="reminder"
                className="w-40"
                inputMode="numeric"
                placeholder="mailbox default"
                value={fields?.reminder ?? ""}
                disabled={fields?.noReminder ?? false}
                onChange={(e) => change({ reminder: e.currentTarget.value })}
              />
              <span className="text-sm text-muted-foreground">minutes before</span>
              {check("noReminder", "None", (on) => change({ noReminder: on }))}
            </span>
          </div>
          {row(
            "body",
            "Description",
            <Textarea id={`${id}-body`} data-field="body" rows={3} value={fields?.body ?? ""} onChange={(e) => change({ body: e.currentTarget.value })} />,
          )}
          <p role="alert" data-slot="event-error" className={`min-h-5 text-sm ${error ? "text-destructive" : "text-warning"}`}>
            {error ?? problem}
          </p>
          <DialogFooter>
            <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
            <Button type="submit" data-submit="" disabled={busy || problem !== null}>
              {retry ? "Retry" : "Create event"} <Kbd aria-hidden="true">⌘↵</Kbd>
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
