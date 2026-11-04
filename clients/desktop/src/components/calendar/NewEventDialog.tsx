// The event form (CAL-11, CAL-12, PERSO-255; the TUI's `n` and `E` overlay,
// the CLI's `mp event create` and `mp event update`): Calendar, Title, All
// day, Start, End, the attendees' availability (CAL-14), Location,
// Attendees, Optional, Rooms, Teams, Reminder, Show as, Description. It
// creates through `event.create`, and a failed or vanished create opens it
// again with what was typed and a Retry that sends the same transaction id.
// Opened on an event (`dialog.edit`), it holds the event's values and Save
// sends `event.update` with what changed; the calendar stays the event's, a
// Teams meeting stays one, and an attendee changes the reminder and the
// show-as only. Its Repeats section makes a series: a pattern read against
// the first day, its interval, a weekly series' days and its end, with the
// rule in words under it. On one occurrence of a series it names the series
// the occurrence stays part of; on the whole series it holds the series'
// rule, a rule the form does not write kept as it is until a pattern
// replaces it.

import { useEffect, useId, useRef, useState, type ComponentProps, type KeyboardEvent, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Textarea } from "@/components/ui/textarea";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { eventProblem, localZone, SHOW_AS, submitEvent } from "@/app/eventForm";
import { editProblem, submitEdit } from "@/app/eventChange";
import { describeRecurrence, NO_REPEAT, patternLabel, REPEAT_PATTERNS, repeatLabel, WEEKDAYS, type RepeatFields, type RepeatPattern } from "@/app/recurrence";
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
  const edit = dialog?.edit ?? null;
  const problem = fields && dialog ? (edit ? editProblem({ ...dialog, fields }) : eventProblem(fields, calendars)) : null;
  const retry = dialog?.transactionId != null;
  // An attendee changes their copy's reminder and show-as and nothing else.
  const locked = edit !== null && !edit.organizer;
  const scope = edit?.scope ?? null;
  const title = edit
    ? locked
      ? "Edit event (your reminder and show-as)"
      : scope === "series"
        ? "Edit the whole series"
        : scope === "occurrence"
          ? "Edit this occurrence"
          : "Edit event"
    : retry
      ? "New event (retry)"
      : "New event";
  // A word the server gave that the list does not name (`unknown`) shows until another is picked.
  const showAs = fields?.showAs ?? "busy";
  const showAsOptions = SHOW_AS.some((o) => o.word === showAs) ? SHOW_AS : [...SHOW_AS, { word: showAs, label: showAs }];

  const submit = async () => {
    if (!dialog || !fields || busy) return;
    setBusy(true);
    const refused = edit ? await submitEdit(dispatch, { ...dialog, fields }) : await submitEvent(dispatch, { ...dialog, fields });
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
        disabled={locked && name !== "reminder"}
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
  const repeat = fields?.repeat ?? NO_REPEAT;
  const day = (fields?.start ?? "").slice(0, 10);
  const setRepeat = (next: Partial<RepeatFields>) => change({ repeat: { ...repeat, ...next } });
  const units: Record<RepeatPattern, string> = { daily: "days", weekly: "weeks", "monthly-date": "months", "monthly-weekday": "months", yearly: "years" };
  const selectClass =
    "h-8 rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50";
  // A new event repeats or not; a series always repeats; an occurrence keeps its series' rule.
  const repeats =
    edit === null || scope !== null
      ? row(
          "repeat",
          "Repeats",
          scope === "occurrence" ? (
            <span data-slot="repeat-series" className="text-sm">
              {repeatLabel(repeat, day)}
            </span>
          ) : (
            <select
              id={`${id}-repeat`}
              data-field="repeat"
              disabled={locked}
              value={repeat.pattern ?? (repeat.kept ? "kept" : "none")}
              onChange={(e) => {
                const value = e.currentTarget.value;
                if (value === "kept") return;
                setRepeat({ pattern: value === "none" ? null : (value as RepeatPattern), kept: null });
              }}
              className={selectClass}
            >
              {scope === null ? <option value="none">Does not repeat</option> : null}
              {repeat.kept ? <option value="kept">{describeRecurrence(repeat.kept)}</option> : null}
              {REPEAT_PATTERNS.map((p) => (
                <option key={p} value={p}>
                  {patternLabel(p, day)}
                </option>
              ))}
            </select>
          ),
        )
      : null;
  const pattern = scope === "occurrence" ? null : repeat.pattern;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={titleRef} data-slot="new-event" className="sm:max-w-lg">
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>
            {edit
              ? `Times in ${localZone()}; only what you change is sent, and the server tells the attendees.`
              : `Times in ${localZone()}; the server sends each attendee and room an invitation.`}
          </DialogDescription>
        </DialogHeader>
        <form
          ref={formRef}
          aria-label={edit ? "Edit event" : "New event"}
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
              disabled={edit !== null}
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
            {check("allDay", "All day", toggleAllDay, locked)}
          </div>
          {field("start", allDay ? "First day" : "Start", { type: allDay ? "date" : "datetime-local" })}
          {field("end", allDay ? "Last day" : "End", { type: allDay ? "date" : "datetime-local" })}
          <AvailabilityGrid fields={fields} calendar={chosen} current={edit?.original ?? null} />
          {field("location", "Location")}
          {field("to", "Attendees", { placeholder: "ada@example.com, bob@example.com" })}
          {field("optional", "Optional")}
          {field("rooms", "Rooms", { placeholder: "room mailboxes" })}
          <div className="grid grid-cols-[6rem_1fr] items-center gap-2">
            <span />
            <span className="flex flex-col gap-0.5">
              {check("teams", "Teams meeting", (on) => change({ teams: on }), !chosen?.teams || locked || edit?.online === true)}
              {edit?.online ? (
                <span data-slot="teams-stays" className="text-xs text-muted-foreground">
                  A Teams meeting stays one
                </span>
              ) : chosen && !chosen.teams ? (
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
                placeholder={edit ? "none" : "mailbox default"}
                value={fields?.reminder ?? ""}
                disabled={fields?.noReminder ?? false}
                onChange={(e) => change({ reminder: e.currentTarget.value })}
              />
              <span className="text-sm text-muted-foreground">minutes before</span>
              {check("noReminder", "None", (on) => change({ noReminder: on }))}
            </span>
          </div>
          {row(
            "show-as",
            "Show as",
            <select
              id={`${id}-show-as`}
              data-field="showAs"
              value={showAs}
              onChange={(e) => change({ showAs: e.currentTarget.value })}
              className="h-8 rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50"
            >
              {showAsOptions.map((o) => (
                <option key={o.word} value={o.word}>
                  {o.label}
                </option>
              ))}
            </select>,
          )}
          {repeats}
          {pattern ? (
            <>
              {row(
                "every",
                "Every",
                <span className="flex items-center gap-2">
                  <Input
                    id={`${id}-every`}
                    data-field="every"
                    className="w-20"
                    inputMode="numeric"
                    value={repeat.every}
                    disabled={locked}
                    onChange={(e) => setRepeat({ every: e.currentTarget.value })}
                  />
                  <span className="text-sm text-muted-foreground">{units[pattern]}</span>
                </span>,
              )}
              {pattern === "weekly"
                ? row(
                    "on",
                    "On",
                    <span id={`${id}-on`} className="flex flex-wrap gap-2" data-slot="repeat-on">
                      {WEEKDAYS.map((weekday) => (
                        <label key={weekday} className="flex items-center gap-1 text-sm">
                          <input
                            type="checkbox"
                            data-weekday={weekday}
                            checked={repeat.on.includes(weekday)}
                            disabled={locked}
                            onChange={(e) =>
                              setRepeat({ on: e.currentTarget.checked ? [...repeat.on, weekday] : repeat.on.filter((d) => d !== weekday) })
                            }
                          />
                          {weekday.charAt(0).toUpperCase() + weekday.slice(1, 3)}
                        </label>
                      ))}
                    </span>,
                  )
                : null}
              {row(
                "ends",
                "Ends",
                <span className="flex items-center gap-2">
                  <select
                    id={`${id}-ends`}
                    data-field="ends"
                    value={repeat.ends}
                    disabled={locked}
                    onChange={(e) => setRepeat({ ends: e.currentTarget.value as RepeatFields["ends"] })}
                    className={selectClass}
                  >
                    <option value="never">Never</option>
                    <option value="until">On</option>
                    <option value="count">After</option>
                  </select>
                  {repeat.ends === "until" ? (
                    <Input
                      type="date"
                      aria-label="Last day"
                      data-field="until"
                      className="w-44"
                      value={repeat.until}
                      disabled={locked}
                      onChange={(e) => setRepeat({ until: e.currentTarget.value })}
                    />
                  ) : repeat.ends === "count" ? (
                    <>
                      <Input
                        aria-label="Occurrences"
                        data-field="count"
                        className="w-20"
                        inputMode="numeric"
                        value={repeat.count}
                        disabled={locked}
                        onChange={(e) => setRepeat({ count: e.currentTarget.value })}
                      />
                      <span className="text-sm text-muted-foreground">times</span>
                    </>
                  ) : null}
                </span>,
              )}
              <p data-slot="repeat-summary" className="pl-[6.5rem] text-xs text-muted-foreground">
                {repeatLabel(repeat, day)}
              </p>
            </>
          ) : null}
          {row(
            "body",
            "Description",
            <Textarea
              id={`${id}-body`}
              data-field="body"
              rows={3}
              disabled={locked || edit?.online === true}
              title={edit?.online ? "A Teams meeting's description cannot be changed from mp yet: the server would drop its join details" : undefined}
              value={fields?.body ?? ""}
              onChange={(e) => change({ body: e.currentTarget.value })}
            />,
          )}
          <p role="alert" data-slot="event-error" className={`min-h-5 text-sm ${error ? "text-destructive" : "text-warning"}`}>
            {error ?? problem}
          </p>
          <DialogFooter>
            <DialogClose render={<Button type="button" variant="outline" />}>Cancel</DialogClose>
            <Button type="submit" data-submit="" disabled={busy || problem !== null}>
              {edit ? "Save changes" : retry ? "Retry" : "Create event"} <Kbd aria-hidden="true">⌘↵</Kbd>
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}
