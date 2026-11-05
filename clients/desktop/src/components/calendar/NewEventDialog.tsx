// The event form (CAL-11, CAL-12, PERSO-255; the TUI's `n` and `E` overlay,
// the CLI's `mp event create` and `mp event update`): Calendar and Title,
// then three sections, When (Start, End, All day, Repeats), People
// (Attendees, Optional, Rooms and their availability, CAL-14) and Details
// (Location, Teams, Reminder, Show as, Description), the body scrolling
// between the header and the buttons on a short window. It creates through `event.create`, and a failed or vanished create opens it
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

import { useEffect, useId, useRef, useState, type ComponentProps, type FocusEvent, type KeyboardEvent, type ReactNode } from "react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import { Textarea } from "@/components/ui/textarea";
import { Dialog, DialogClose, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { eventIssue, localZone, SHOW_AS, submitEvent, type ProblemField } from "@/app/eventForm";
import { editIssue, submitEdit } from "@/app/eventChange";
import { describeRecurrence, NO_REPEAT, patternLabel, REPEAT_PATTERNS, repeatLabel, WEEKDAYS, type RepeatFields, type RepeatPattern } from "@/app/recurrence";
import { AvailabilityGrid } from "@/components/calendar/AvailabilityGrid";
import { useDispatch } from "@/app/store";
import type { EventDialog, EventFields } from "@/app/state";

type TextField = "title" | "start" | "end" | "location" | "to" | "optional" | "rooms" | "reminder" | "body";

export type NewEventDialogProps = { dialog: EventDialog | null; onOpenChange: (open: boolean) => void };

/**
 * The problems a field the user has left reveals: the start and the end
 * reveal each other's, since moving one can put the other wrong, and the
 * rule's, which reads its first day from the start (a series' last day can
 * fall before it); every control of the Repeats section reveals the rule's.
 */
const REVEALS: Record<string, ProblemField[]> = {
  calendar: ["calendar"],
  title: ["title"],
  allDay: ["start", "end", "repeat"],
  start: ["start", "end", "repeat"],
  end: ["start", "end", "repeat"],
  to: ["to"],
  optional: ["optional"],
  rooms: ["rooms"],
  reminder: ["reminder"],
  noReminder: ["reminder"],
  repeat: ["repeat"],
  every: ["repeat"],
  weekday: ["repeat"],
  ends: ["repeat"],
  until: ["repeat"],
  count: ["repeat"],
};

const SELECT =
  "h-8 rounded-lg border border-input bg-transparent px-2 text-sm outline-none focus-visible:border-ring focus-visible:ring-3 focus-visible:ring-ring/50 disabled:cursor-not-allowed disabled:opacity-50";

/**
 * Enter in a one-line field moves to the next enabled one, Cmd+Enter or Ctrl+Enter
 * creates, Escape cancels. The first problem with the fields as typed shows
 * in its alert once the field it is about was left or a create was tried,
 * the daemon's refusal at once, and the form stays open with what was typed.
 */
export function NewEventDialog({ dialog, onOpenChange }: NewEventDialogProps) {
  const dispatch = useDispatch();
  const id = useId();
  const calendars = dialog?.calendars ?? [];
  const [fields, setFields] = useState<EventFields | null>(dialog?.fields ?? null);
  const [error, setError] = useState<string | null>(dialog?.error ?? null);
  const [busy, setBusy] = useState(false);
  const [revealed, setRevealed] = useState<ReadonlySet<ProblemField>>(new Set());
  const [attempted, setAttempted] = useState(false);
  const formRef = useRef<HTMLFormElement>(null);
  const titleRef = useRef<HTMLInputElement>(null);
  const open = dialog !== null;

  useEffect(() => {
    setFields(dialog?.fields ?? null);
    setError(dialog?.error ?? null);
    setBusy(false);
    setRevealed(new Set());
    setAttempted(false);
  }, [dialog]);

  const chosen = calendars.find((c) => c.key === fields?.calendar) ?? null;
  const edit = dialog?.edit ?? null;
  const issue = fields && dialog ? (edit ? editIssue({ ...dialog, fields }) : eventIssue(fields, calendars)) : null;
  const problem = issue?.message ?? null;
  // A problem waits until its field was left or a create was tried: a fresh
  // form is not wrong yet, and the disabled button already says it is not ready.
  const shown = issue && (attempted || (issue.field !== null && revealed.has(issue.field))) ? issue : null;
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
    setAttempted(true);
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
    // A disabled control takes no focus, so Enter would stop on it.
    const order = [...(formRef.current?.querySelectorAll<HTMLElement>("[data-field]:not(:disabled), [data-submit]:not(:disabled)") ?? [])];
    order[order.indexOf(e.target) + 1]?.focus();
  };

  const onBlur = (e: FocusEvent<HTMLFormElement>) => {
    const target = e.target as HTMLElement;
    const reveals = REVEALS[target.dataset.field ?? (target.dataset.weekday ? "weekday" : "")];
    if (!reveals || reveals.every((f) => revealed.has(f))) return;
    setRevealed(new Set([...revealed, ...reveals]));
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
    <div className="grid grid-cols-[6rem_minmax(0,1fr)] items-center gap-2">
      <label htmlFor={`${id}-${name}`} className="text-sm text-muted-foreground">
        {label}
      </label>
      {input}
    </div>
  );
  /** A row whose left column names no single control. */
  const plain = (label: string, content: ReactNode) => (
    <div className="grid grid-cols-[6rem_minmax(0,1fr)] items-center gap-2">
      <span className="text-sm text-muted-foreground">{label}</span>
      {content}
    </div>
  );
  const invalid = (name: string) => (shown?.field === name ? true : undefined);
  const input = (name: TextField, extra: ComponentProps<typeof Input> = {}) => (
    <Input
      id={`${id}-${name}`}
      ref={name === "title" ? titleRef : undefined}
      data-field={name}
      value={fields?.[name] ?? ""}
      autoComplete="off"
      spellCheck={name === "title" || name === "location"}
      disabled={locked && name !== "reminder"}
      aria-invalid={invalid(name)}
      onChange={(e) => change({ [name]: e.currentTarget.value })}
      {...extra}
    />
  );
  const field = (name: TextField, label: string, extra: ComponentProps<typeof Input> = {}) => row(name, label, input(name, extra));
  const check = (name: "allDay" | "teams" | "noReminder", label: string, onChange: (on: boolean) => void, disabled = false) => (
    <label className={`flex shrink-0 items-center gap-2 text-sm ${disabled ? "text-muted-foreground" : ""}`}>
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
  const section = (legend: string, children: ReactNode) => (
    <fieldset className="flex min-w-0 flex-col gap-2">
      <legend className="mb-2 text-xs font-semibold tracking-wide text-muted-foreground uppercase">{legend}</legend>
      {children}
    </fieldset>
  );

  const allDay = fields?.allDay ?? false;
  const repeat = fields?.repeat ?? NO_REPEAT;
  const day = (fields?.start ?? "").slice(0, 10);
  const setRepeat = (next: Partial<RepeatFields>) => change({ repeat: { ...repeat, ...next } });
  const units: Record<RepeatPattern, string> = { daily: "days", weekly: "weeks", "monthly-date": "months", "monthly-weekday": "months", yearly: "years" };
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
              className={SELECT}
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
  const teamsOff = !chosen?.teams || locked || edit?.online === true;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={titleRef} data-slot="new-event" className="max-h-[calc(100dvh-2rem)] grid-rows-[auto_minmax(0,1fr)] sm:max-w-xl">
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
          className="flex min-h-0 flex-col gap-2"
          onKeyDown={onKeyDown}
          onBlur={onBlur}
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <div className="-mx-4 flex min-h-0 flex-col gap-5 overflow-y-auto px-4 pt-1 pb-2">
            <div className="flex flex-col gap-2">
              {row(
                "calendar",
                "Calendar",
                <span className="relative flex min-w-0">
                  <span
                    aria-hidden="true"
                    data-slot="calendar-swatch"
                    className={`pointer-events-none absolute top-1/2 left-2.5 size-2.5 -translate-y-1/2 rounded-full ${edit !== null ? "opacity-50" : ""}`}
                    style={{ backgroundColor: chosen?.color ?? "var(--color-link)" }}
                  />
                  <select
                    id={`${id}-calendar`}
                    data-field="calendar"
                    disabled={edit !== null}
                    value={fields?.calendar ?? ""}
                    onChange={(e) => {
                      const next = calendars.find((c) => c.key === e.currentTarget.value);
                      change({ calendar: e.currentTarget.value, teams: next?.teams ? (fields?.teams ?? false) : false });
                    }}
                    className={`${SELECT} w-full min-w-0 pl-7`}
                  >
                    {calendars.map((c) => (
                      <option key={c.key} value={c.key}>
                        {`${c.name} · ${c.account}`}
                      </option>
                    ))}
                  </select>
                </span>,
              )}
              {field("title", "Title", { placeholder: "Add a title" })}
            </div>
            {section(
              "When",
              <>
                {row(
                  "start",
                  allDay ? "First day" : "Start",
                  <span className="flex items-center gap-4">
                    {input("start", { type: allDay ? "date" : "datetime-local", className: "w-60" })}
                    {check("allDay", "All day", toggleAllDay, locked)}
                  </span>,
                )}
                {row("end", allDay ? "Last day" : "End", <span className="flex">{input("end", { type: allDay ? "date" : "datetime-local", className: "w-60" })}</span>)}
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
                          <span id={`${id}-on`} className="flex flex-wrap gap-x-3 gap-y-1" data-slot="repeat-on">
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
                          className={SELECT}
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
              </>,
            )}
            {section(
              "People",
              <>
                {field("to", "Attendees", { placeholder: "Email addresses, comma-separated" })}
                {field("optional", "Optional", { placeholder: "Email addresses, comma-separated" })}
                {field("rooms", "Rooms", { placeholder: "Room email addresses" })}
                <AvailabilityGrid fields={fields} calendar={chosen} current={edit?.original ?? null} />
              </>,
            )}
            {section(
              "Details",
              <>
                {field("location", "Location", { placeholder: "Room, address or link" })}
                {plain(
                  "Online",
                  <span className="flex min-w-0 items-center gap-3">
                    {check("teams", "Teams meeting", (on) => change({ teams: on }), teamsOff)}
                    {edit?.online ? (
                      <span data-slot="teams-stays" className="truncate text-xs text-muted-foreground">
                        A Teams meeting stays one
                      </span>
                    ) : chosen && !chosen.teams ? (
                      <span data-slot="teams-unavailable" className="truncate text-xs text-muted-foreground">
                        This calendar has no online meetings
                      </span>
                    ) : null}
                  </span>,
                )}
                {row(
                  "reminder",
                  "Reminder",
                  <span className="flex items-center gap-4">
                    <span className="flex items-center gap-2">
                      <Input
                        id={`${id}-reminder`}
                        data-field="reminder"
                        className="w-20 text-right tabular-nums"
                        inputMode="numeric"
                        placeholder={fields?.noReminder ? undefined : edit ? "None" : "Default"}
                        title={edit ? "Empty means no reminder" : "Empty uses the mailbox default"}
                        value={fields?.reminder ?? ""}
                        disabled={fields?.noReminder ?? false}
                        aria-invalid={invalid("reminder")}
                        onChange={(e) => change({ reminder: e.currentTarget.value })}
                      />
                      <span className={`text-sm ${fields?.noReminder ? "text-muted-foreground/60" : "text-muted-foreground"}`}>minutes before</span>
                    </span>
                    {check("noReminder", "None", (on) => change({ noReminder: on }))}
                  </span>,
                )}
                {row(
                  "show-as",
                  "Show as",
                  <select
                    id={`${id}-show-as`}
                    data-field="showAs"
                    value={showAs}
                    onChange={(e) => change({ showAs: e.currentTarget.value })}
                    className={SELECT}
                  >
                    {showAsOptions.map((o) => (
                      <option key={o.word} value={o.word}>
                        {o.label}
                      </option>
                    ))}
                  </select>,
                )}
                <div className="grid grid-cols-[6rem_minmax(0,1fr)] items-start gap-2">
                  <label htmlFor={`${id}-body`} className="pt-1.5 text-sm text-muted-foreground">
                    Description
                  </label>
                  <Textarea
                    id={`${id}-body`}
                    data-field="body"
                    rows={3}
                    disabled={locked || edit?.online === true}
                    title={edit?.online ? "A Teams meeting's description cannot be changed from mp yet: the server would drop its join details" : undefined}
                    value={fields?.body ?? ""}
                    onChange={(e) => change({ body: e.currentTarget.value })}
                  />
                </div>
              </>,
            )}
          </div>
          <p role="alert" data-slot="event-error" className={`min-h-5 text-sm ${error ? "text-destructive" : "text-warning"}`}>
            {error ?? shown?.message ?? null}
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
