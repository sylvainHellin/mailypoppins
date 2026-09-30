import { useCallback, useRef } from "react";
import { ArrowLeft, CalendarCheck, CalendarPlus, History, RotateCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { AgendaList } from "@/components/calendar/AgendaList";
import { EventCard } from "@/components/calendar/EventCard";
import { runAction } from "@/app/actions";
import { calendarRows, cursorEvent } from "@/app/calendar";
import { agendaRsvpRefusal, rsvpOf } from "@/app/rsvp";
import { useAppState, useDispatch } from "@/app/store";

/** The daemon's `AccountNotReady`: the account has no local store to read yet. */
const ACCOUNT_NOT_READY = -32006;

/**
 * The Calendar view (clients/desktop/docs/shell.md, "Calendar"): the
 * selection account's agenda, upcoming events only until `t` shows the past
 * ones, and the event card of the row under the cursor. Enter or `e` opens
 * the entry's invite.ics in the editor, `r` reads the agenda again, Escape
 * brings Mail back.
 */
export function CalendarView() {
  const s = useAppState();
  const dispatch = useDispatch();
  const view = s.calendarView;
  const account = view?.account ?? null;
  const l = account ? s.calendar[account] : undefined;
  const events = l?.data ?? null;
  const rows = calendarRows(s);
  const current = cursorEvent(s, rows);
  const showPast = view?.showPast ?? false;
  const run = (id: "calendar_toggle_past" | "calendar_refresh" | "calendar_rsvp" | "new_invitation") => runAction(id, s, dispatch);
  const refusal = current && account ? (agendaRsvpRefusal(current, s.inviteRefusals[account] ?? null) ?? (rsvpOf(s, account, current.row_id) ? "A reply to this invitation is being sent" : null)) : null;
  const select = useCallback((rowId: number) => dispatch({ type: "calendar_select", row_id: rowId }), [dispatch]);
  const latest = useRef(s);
  latest.current = s;
  // A double-click opens the row clicked, which the model's cursor may not be yet.
  const open = useCallback(
    (rowId: number) => {
      dispatch({ type: "calendar_select", row_id: rowId });
      const now = latest.current;
      const v = now.calendarView;
      if (v) runAction("calendar_open_source", { ...now, calendarView: { ...v, cursor: rowId } }, dispatch);
    },
    [dispatch],
  );
  const narrow = s.layout === "narrow";
  const scope = showPast ? "events, past included" : "upcoming events";

  return (
    <section
      aria-label="Calendar"
      className="flex h-full min-h-0 min-w-0 flex-col"
      data-pane="list"
      data-view="calendar"
      onFocus={() => dispatch({ type: "pane_focused", pane: "list" })}
    >
      <header className="flex shrink-0 flex-col gap-1 border-b border-border px-3 py-2">
        <div className="flex items-center justify-between gap-2">
          <h2 className="truncate text-sm font-semibold">Calendar</h2>
          <span className="flex shrink-0 gap-1">
            <Button size="xs" variant="ghost" onClick={() => run("new_invitation")} title="New invitation" disabled={!view}>
              <CalendarPlus aria-hidden="true" />
              New invitation
            </Button>
            <Button size="xs" variant="ghost" aria-pressed={showPast} onClick={() => run("calendar_toggle_past")} title="Show past events / upcoming only (t)">
              <History aria-hidden="true" />
              Past events
            </Button>
            <Button size="xs" variant="ghost" onClick={() => run("calendar_refresh")} title="Refresh events (r)" disabled={!view}>
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
          {account ? `${account}: ${events ? `${rows.length} ${scope}` : "loading"}` : "\u00a0"}
        </p>
      </header>
      <div className={`flex min-h-0 flex-1 ${narrow ? "flex-col overflow-y-auto" : ""}`}>
        <div className={`min-w-0 ${narrow ? "" : "min-h-0 flex-1 overflow-y-auto border-r border-border"}`}>
          {!account ? (
            <p className="p-4 text-sm text-muted-foreground">No account is selected.</p>
          ) : l?.error && !events ? (
            "code" in l.error && l.error.code === ACCOUNT_NOT_READY ? (
              <p className="p-4 text-sm text-muted-foreground">
                {account} has no local store yet, so it has no agenda; it appears after the account's first sync.
              </p>
            ) : (
              <p role="alert" className="p-4 text-sm text-destructive">
                The agenda did not load: {l.error.message}
              </p>
            )
          ) : !events ? (
            <div aria-busy="true" aria-label="Loading the agenda" className="flex flex-col gap-2 p-3">
              {[0, 1, 2].map((i) => (
                <Skeleton key={i} className="h-9 w-full" />
              ))}
            </div>
          ) : rows.length === 0 ? (
            <p className="p-4 text-sm text-muted-foreground">
              {events.length === 0
                ? "No invitations found in this account's mail; r reads the agenda again."
                : "No upcoming events; t shows the past ones."}
            </p>
          ) : (
            <AgendaList rows={rows} cursor={current?.row_id ?? null} focusSeq={s.focusSeq} onSelect={select} onOpen={open} />
          )}
          <p className="px-3 py-2 text-xs text-muted-foreground">Only events that arrived by email are listed.</p>
        </div>
        <div className={`min-w-0 p-4 ${narrow ? "border-t border-border" : "min-h-0 flex-1 overflow-y-auto"}`}>
          {current ? (
            <EventCard event={current.event} organizer={current.is_organizer}>
              <div className="flex flex-col items-start gap-1 pt-1">
                <Button size="sm" variant="outline" disabled={refusal !== null} onClick={() => run("calendar_rsvp")} title="RSVP to invitation (V)">
                  <CalendarCheck aria-hidden="true" />
                  RSVP
                </Button>
                {refusal ? (
                  <p data-slot="rsvp-refusal" className="text-xs text-muted-foreground">
                    {refusal}
                  </p>
                ) : null}
              </div>
            </EventCard>
          ) : (
            <p className="text-sm text-muted-foreground">Select an event to see its details.</p>
          )}
        </div>
      </div>
    </section>
  );
}
