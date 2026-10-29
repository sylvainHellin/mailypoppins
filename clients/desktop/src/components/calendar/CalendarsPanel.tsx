import { useId } from "react";
import { setCalendarVisible } from "@/app/calendar";
import type { Action } from "@/app/reducer";
import type { Loadable } from "@/app/state";
import type { CalendarInfo } from "@/protocol/types";

export type CalendarsPanelProps = {
  calendars: Loadable<CalendarInfo[]>;
  dispatch: (a: Action) => void;
};

/**
 * The calendars shown or hidden (`c`, the TUI's calendars overlay): every
 * account's calendars from `calendar_list`, a checkbox each, which calls
 * `calendar_set_visibility`; the daemon's `calendar.changed` then reads the
 * view and this list again. The preference is the daemon's, kept across a
 * cache rebuild, so the panel keeps nothing of its own.
 */
export function CalendarsPanel({ calendars, dispatch }: CalendarsPanelProps) {
  const id = useId();
  const list = calendars.data;
  const accounts = [...new Set((list ?? []).map((c) => c.account))];
  return (
    <section aria-labelledby={id} data-slot="calendars-panel" data-own-keys className="flex flex-col gap-2 border-b border-border px-3 py-2 text-sm">
      <h3 id={id} className="text-xs font-semibold text-muted-foreground">
        Calendars
      </h3>
      {list === null ? (
        calendars.error ? (
          <p role="alert" className="text-destructive">
            The calendars did not load: {calendars.error.message}
          </p>
        ) : (
          <p className="text-muted-foreground">Loading the calendars…</p>
        )
      ) : list.length === 0 ? (
        <p className="text-muted-foreground">No calendars: no account has a server calendar or an invitation.</p>
      ) : (
        <div className="flex flex-wrap gap-x-6 gap-y-2">
          {accounts.map((account) => (
            <fieldset key={account} className="flex flex-col gap-1">
              <legend className="text-xs text-muted-foreground">{account}</legend>
              {list
                .filter((c) => c.account === account)
                .map((c) => (
                  <label key={c.id} className="flex items-center gap-2">
                    <input
                      type="checkbox"
                      checked={c.visible}
                      aria-label={`${c.account}/${c.name}`}
                      data-calendar={`${c.account}/${c.id}`}
                      onChange={(e) => void setCalendarVisible(dispatch, c, e.currentTarget.checked)}
                    />
                    <span aria-hidden="true" className="h-3 w-3 rounded-sm border border-border" style={c.color ? { backgroundColor: c.color } : undefined} />
                    <span>{c.name}</span>
                    {c.is_default ? <span className="text-xs text-muted-foreground">(default)</span> : null}
                  </label>
                ))}
            </fieldset>
          ))}
        </div>
      )}
    </section>
  );
}
