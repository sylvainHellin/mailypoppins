import { useId } from "react";
import { Check } from "lucide-react";
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
 *
 * Each row is one button with the checkbox role, as the list's mark box is:
 * the box is the calendar's swatch, filled with its colour (the link colour
 * when it has none) while shown and outlined in it while hidden, so the
 * colour and the state read from one mark. A button toggles on Enter and
 * Space alike, and the section's `data-own-keys` keeps the view's bindings
 * off those two keys.
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
        <div className="flex flex-wrap gap-x-6 gap-y-1">
          {accounts.map((account) => (
            <fieldset key={account} className="flex min-w-0 flex-col">
              <legend className="mb-0.5 px-1.5 text-xs text-muted-foreground">{account}</legend>
              {list
                .filter((c) => c.account === account)
                .map((c) => (
                  <CalendarToggle key={c.id} calendar={c} onToggle={(visible) => void setCalendarVisible(dispatch, c, visible)} />
                ))}
            </fieldset>
          ))}
        </div>
      )}
    </section>
  );
}

function CalendarToggle({ calendar: c, onToggle }: { calendar: CalendarInfo; onToggle: (visible: boolean) => void }) {
  const tint = c.color ?? "var(--color-link)";
  return (
    <button
      type="button"
      role="checkbox"
      aria-checked={c.visible}
      aria-label={`${c.account}/${c.name}`}
      data-calendar={`${c.account}/${c.id}`}
      onClick={() => onToggle(!c.visible)}
      className="flex min-w-0 cursor-pointer items-center gap-2 rounded-sm px-1.5 py-0.5 text-left outline-none hover:bg-accent/60 focus-visible:outline-2 focus-visible:outline-ring"
    >
      <span
        aria-hidden="true"
        className="inline-flex size-3.5 shrink-0 items-center justify-center rounded-sm border text-background"
        style={{ borderColor: tint, backgroundColor: c.visible ? tint : "transparent" }}
      >
        {c.visible ? <Check className="size-3" strokeWidth={3} /> : null}
      </span>
      <span className={`truncate ${c.visible ? "" : "text-muted-foreground"}`}>{c.name}</span>
      {c.is_default ? <span className="shrink-0 text-xs text-muted-foreground">default</span> : null}
    </button>
  );
}
