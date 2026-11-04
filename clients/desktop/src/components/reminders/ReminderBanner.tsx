// The calendar reminders that fired and wait (CAL-15, PERSO-255), one line
// each above the panes, over every view: the summary, which opens the event
// in the Calendar view, when and where, and Snooze and Dismiss. The system
// notification is the Rust layer's, and macOS shows none while this window
// is frontmost, so the banner is what says a reminder fired whatever the
// notification did. A decision goes to the daemon, whose
// `calendar.reminders_changed` clears or moves the reminder in every client.

import { useEffect, useState } from "react";
import { AlarmClock, BellOff, Clock } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useAppState, useDispatch } from "@/app/store";
import { reminderDetail, SNOOZE_MINUTES } from "@/app/reminders";
import { reminderDismiss, reminderSnooze } from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";
import type { CalendarReminder } from "@/protocol/types";

/** Re-render once a minute, so `in 5 min` keeps counting down. */
function useMinute(): Date {
  const [now, setNow] = useState(() => new Date());
  useEffect(() => {
    const timer = setInterval(() => setNow(new Date()), 30_000);
    return () => clearInterval(timer);
  }, []);
  return now;
}

export function ReminderBanner() {
  const s = useAppState();
  const dispatch = useDispatch();
  const now = useMinute();
  const reminders = s.reminders.data ?? [];
  if (reminders.length === 0) return null;

  const title = (r: CalendarReminder) => (r.summary.trim() === "" ? "(no title)" : r.summary);
  const decide = (r: CalendarReminder, snooze: boolean) => {
    const done = snooze ? reminderSnooze(r.account, r.event, SNOOZE_MINUTES) : reminderDismiss(r.account, r.event);
    done
      .then(() => {
        dispatch({ type: "reminder_decided", account: r.account, event: r.event });
        dispatch({ type: "notice", text: snooze ? `Snoozed ${title(r)} for ${SNOOZE_MINUTES} min` : `Dismissed ${title(r)}` });
      })
      .catch((e: unknown) =>
        dispatch({
          type: "notice",
          text: `Cannot ${snooze ? "snooze" : "dismiss"} ${title(r)}: ${asGuiError(e).message}`,
          level: "error",
        }),
      );
  };

  return (
    <section aria-label="Reminders" data-slot="reminders" className="border-b border-border bg-card">
      <ul className="flex flex-col">
        {reminders.map((r) => (
          <li key={`${r.account}\u0000${r.event}`} className="flex items-center gap-2 px-4 py-1.5 text-sm">
            <AlarmClock aria-hidden="true" className="size-4 shrink-0 text-warning" />
            <button
              type="button"
              className="min-w-0 flex-1 truncate text-left hover:underline"
              title={`${r.account}: open the event`}
              aria-label={`Open ${title(r)}, ${reminderDetail(r, now)}, in the Calendar view`}
              onClick={() => dispatch({ type: "reminder_open", account: r.account, event: r.event, start: r.start })}
            >
              <span className="font-medium">{title(r)}</span>
              <span className="text-muted-foreground">, {reminderDetail(r, now)}</span>
            </button>
            <Button size="xs" variant="outline" onClick={() => decide(r, true)} aria-label={`Snooze ${title(r)} for ${SNOOZE_MINUTES} minutes`}>
              <Clock aria-hidden="true" />
              Snooze {SNOOZE_MINUTES} min
            </Button>
            <Button size="xs" variant="outline" onClick={() => decide(r, false)} aria-label={`Dismiss ${title(r)}`}>
              <BellOff aria-hidden="true" />
              Dismiss
            </Button>
          </li>
        ))}
      </ul>
    </section>
  );
}
