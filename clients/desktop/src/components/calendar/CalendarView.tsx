import { useCallback, useState, type FormEvent } from "react";
import { ArrowLeft, CalendarCheck, CalendarPlus, ChevronLeft, ChevronRight, Layers, RotateCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { AgendaList } from "@/components/calendar/AgendaList";
import { CalendarGrid } from "@/components/calendar/CalendarGrid";
import { CalendarEventCard } from "@/components/calendar/EventCard";
import { CalendarsPanel } from "@/components/calendar/CalendarsPanel";
import { runAction } from "@/app/actions";
import {
  CALENDAR_CARD_ID,
  CALENDAR_JUMP_ID,
  calendarRows,
  cursorEntry,
  daysBetween,
  layoutDays,
  loadIsFor,
  openEventMail,
  parseDay,
  rangeLabel,
  today as todayOf,
  wantedDays,
} from "@/app/calendar";
import { rsvpDialogFor } from "@/app/rsvp";
import { useAppState, useDispatch } from "@/app/store";
import type { CalendarLayout } from "@/app/state";
import type { ActionId } from "@/keymap/catalog";
import * as cmd from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";
import { usePaneFocused } from "@/hooks/use-pane-focused";

const LAYOUTS: { layout: CalendarLayout; label: string; key: string; action: ActionId }[] = [
  { layout: "agenda", label: "Agenda", key: "a", action: "calendar_layout_agenda" },
  { layout: "day", label: "Day", key: "d", action: "calendar_layout_day" },
  { layout: "week", label: "Week", key: "w", action: "calendar_layout_week" },
];

/**
 * The Calendar view (clients/desktop/docs/shell.md, "Calendar", PERSO-200):
 * every account's events from `calendar_view`, in the agenda, the day grid
 * or the week grid, beside the card of the event under the cursor. `h`/`l`
 * move by a day or a week, `t` comes back to today, `gt` jumps to a date,
 * `a`/`d`/`w` switch the layout, `c` shows the calendars to show or hide,
 * `V` answers the cursor event, `e` opens an invitation's invite.ics, `m`
 * its mail, `r` reads the events again, Escape brings Mail back.
 */
export function CalendarView() {
  const s = useAppState();
  const dispatch = useDispatch();
  const focused = usePaneFocused("list");
  const view = s.calendarView;
  const [jump, setJump] = useState("");
  const run = (id: ActionId) => runAction(id, s, dispatch);
  const select = useCallback((key: string) => dispatch({ type: "calendar_select", key }), [dispatch]);
  // A double-click selects the event and takes the focus to its card, as Enter does.
  const open = useCallback(
    (key: string) => {
      dispatch({ type: "calendar_select", key });
      document.getElementById(CALENDAR_CARD_ID)?.focus();
    },
    [dispatch],
  );
  if (!view) return null;
  const rows = calendarRows(s);
  const current = cursorEntry(s, rows);
  const calendars = s.calendars.data;
  const load = s.calendar;
  const ready = loadIsFor(load, wantedDays(s));
  const now = todayOf();
  const { first, last } = layoutDays(view.layout, view.anchor);
  const narrow = s.layout === "narrow";
  const answer = current ? rsvpDialogFor(s, current.event) : null;
  const refusal = typeof answer === "string" ? answer : null;

  const submitJump = (e: FormEvent) => {
    e.preventDefault();
    const day = parseDay(jump);
    if (!day) {
      dispatch({ type: "notice", text: `Not a date: ${jump.trim() || "(empty)"}; type YYYY-MM-DD` });
      return;
    }
    setJump("");
    dispatch({ type: "calendar_go_to", day });
    dispatch({ type: "focus", pane: "list" });
  };

  const body = () => {
    if (load.error && !load.data) {
      return (
        <p role="alert" className="p-4 text-sm text-destructive">
          The calendar did not load: {load.error.message}
        </p>
      );
    }
    if (!load.data) {
      return (
        <div aria-busy="true" aria-label="Loading the calendar" className="flex flex-col gap-2 p-3">
          {[0, 1, 2].map((i) => (
            <Skeleton key={i} className="h-9 w-full" />
          ))}
        </div>
      );
    }
    if (view.layout === "agenda") {
      return rows.length === 0 ? (
        <p className="p-4 text-sm text-muted-foreground">
          {ready ? "No events in these days; l and h move a week, t comes back to today, g t jumps to a date." : "Loading these days…"}
        </p>
      ) : (
        <AgendaList entries={rows} calendars={calendars} cursor={current?.key ?? null} today={now} focusSeq={s.focusSeq} onSelect={select} onOpen={open} />
      );
    }
    return (
      <CalendarGrid
        label={view.layout === "day" ? "Day" : "Week"}
        days={daysBetween(first, last)}
        entries={rows}
        calendars={calendars}
        cursor={current?.key ?? null}
        today={now}
        focusSeq={s.focusSeq}
        onSelect={select}
        onOpen={open}
      />
    );
  };

  return (
    <section
      aria-label="Calendar"
      className="flex h-full min-h-0 min-w-0 flex-col"
      data-pane="list"
      data-focused={focused}
      data-view="calendar"
      data-layout={view.layout}
      onFocus={() => dispatch({ type: "pane_focused", pane: "list" })}
    >
      <header className="flex shrink-0 flex-col gap-1 border-b border-border px-3 py-2">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <h2 className="truncate text-sm font-semibold">Calendar</h2>
          <span className="flex shrink-0 flex-wrap items-center gap-1">
            <Button size="xs" variant="ghost" onClick={() => run("calendar_prev")} title="Previous day or week (h)" aria-label="Previous">
              <ChevronLeft aria-hidden="true" />
            </Button>
            <Button size="xs" variant="ghost" onClick={() => run("calendar_today")} title="Go to today (t)">
              Today
            </Button>
            <Button size="xs" variant="ghost" onClick={() => run("calendar_next")} title="Next day or week (l)" aria-label="Next">
              <ChevronRight aria-hidden="true" />
            </Button>
            <form onSubmit={submitJump} className="flex items-center">
              <Input
                id={CALENDAR_JUMP_ID}
                aria-label="Jump to date"
                placeholder="YYYY-MM-DD (g t)"
                value={jump}
                onChange={(e) => setJump(e.currentTarget.value)}
                className="h-6 w-36 text-xs"
              />
            </form>
            <span role="group" aria-label="Layout" className="flex">
              {LAYOUTS.map((l) => (
                <Button
                  key={l.layout}
                  size="xs"
                  variant="ghost"
                  aria-pressed={view.layout === l.layout}
                  className="aria-pressed:bg-selection aria-pressed:text-selection-foreground"
                  onClick={() => run(l.action)}
                  title={`${l.label} layout (${l.key})`}
                >
                  {l.label}
                </Button>
              ))}
            </span>
            <Button size="xs" variant="ghost" aria-pressed={view.calendarsOpen} onClick={() => run("calendar_calendars")} title="Show or hide calendars (c)">
              <Layers aria-hidden="true" />
              Calendars
            </Button>
            <Button size="xs" variant="ghost" onClick={() => run("new_invitation")} title="New invitation">
              <CalendarPlus aria-hidden="true" />
              New invitation
            </Button>
            <Button size="xs" variant="ghost" onClick={() => run("calendar_refresh")} title="Read the events again (r)">
              <RotateCw aria-hidden="true" />
              Refresh
            </Button>
            <Button size="xs" variant="ghost" onClick={() => dispatch({ type: "switch_view", view: "mail" })} title="Back to Mail (Esc)">
              <ArrowLeft aria-hidden="true" />
              Mail
            </Button>
          </span>
        </div>
        <p data-slot="calendar-scope" className="text-xs text-muted-foreground tabular-nums">
          {rangeLabel(view.layout, view.anchor)}
          {ready ? `: ${rows.length} ${rows.length === 1 ? "event" : "events"}` : load.data ? ": loading" : ""}
        </p>
      </header>
      {view.calendarsOpen ? <CalendarsPanel calendars={s.calendars} dispatch={dispatch} /> : null}
      <div className={`flex min-h-0 flex-1 ${narrow ? "flex-col overflow-y-auto" : ""}`}>
        {/* With no event under the cursor the body itself takes the focus, so the keys reach the view and not a toolbar button. */}
        <div
          tabIndex={-1}
          data-roving={current ? undefined : "active"}
          className={`min-w-0 outline-none ${narrow ? "" : "min-h-0 flex-1 overflow-y-auto border-r border-border"}`}
        >
          {body()}
        </div>
        <div className={`min-w-0 p-4 ${narrow ? "border-t border-border" : "min-h-0 w-80 shrink-0 overflow-y-auto"}`}>
          {current ? (
            <CalendarEventCard
              id={CALENDAR_CARD_ID}
              event={current.event}
              calendars={calendars}
              onOpenMail={() => openEventMail(current.event, dispatch)}
              onJoin={(url) =>
                void cmd.openExternal(url).catch((e: unknown) => dispatch({ type: "notice", text: `Could not open the meeting link: ${asGuiError(e).message}` }))
              }
            >
              <div className="flex flex-col items-start gap-1 pt-1">
                <Button size="sm" variant="outline" disabled={refusal !== null} onClick={() => run("calendar_rsvp")} title="Respond (V)">
                  <CalendarCheck aria-hidden="true" />
                  Respond
                </Button>
                {refusal ? (
                  <p data-slot="rsvp-refusal" className="text-xs text-muted-foreground">
                    {refusal}
                  </p>
                ) : null}
              </div>
            </CalendarEventCard>
          ) : (
            <p className="text-sm text-muted-foreground">Select an event to see its details.</p>
          )}
        </div>
      </div>
    </section>
  );
}
