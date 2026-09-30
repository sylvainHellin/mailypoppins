import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Input } from "@/components/ui/input";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { ACTIVITY_LOG_CAP, entryTime, filterLog } from "@/app/activity";
import type { ActivityLevel } from "@/app/state";
import { useAppState } from "@/app/store";
import { isEditable } from "@/keymap/useKeymap";

export type ActivityLogDialogProps = { open: boolean; onOpenChange: (open: boolean) => void };

/** One line's scroll step, the height of a row. */
const LINE_PX = 28;
/** How long a `g` waits for the second `g`. */
const PREFIX_TIMEOUT_MS = 1200;

/** Each level's colour token: information in `link`, a warning in `warning`, an error in `destructive`. */
const LEVEL_CLASS: Record<ActivityLevel, string> = {
  info: "text-link",
  warning: "text-warning",
  error: "text-destructive",
};

/**
 * The activity log (`sl`, the sidebar's Activity entry), the TUI's ACTIVITY
 * LOG overlay: every line this window reported, oldest first and scrolled to
 * the newest, each with its time and level. `j`/`k` scroll a line, `d`/`u`
 * half the view, `gg` and `G` to the top and the end, `/` goes to the filter
 * field, Escape closes. In the field Enter goes back to the lines with the
 * filter kept, Escape clears the filter, or closes the dialog when it is
 * already empty (the TUI's). The dialog owns every key while it is open.
 */
export function ActivityLogDialog({ open, onOpenChange }: ActivityLogDialogProps) {
  const s = useAppState();
  const id = useId();
  const logRef = useRef<HTMLDivElement>(null);
  const filterRef = useRef<HTMLInputElement>(null);
  const [filter, setFilter] = useState("");
  const atEnd = useRef(true);
  const shown = filterLog(s.activityLog, filter);

  // A new dialog starts unfiltered, at the end.
  useEffect(() => {
    if (open) {
      setFilter("");
      atEnd.current = true;
    }
  }, [open]);

  // Scrolled to the newest line when it opens (the popup mounts after the
  // dialog's first render, so on the element's mount), when the filter
  // changes, and when a line arrives while the end is in view.
  const setLog = useCallback((el: HTMLDivElement | null) => {
    logRef.current = el;
    if (el) el.scrollTop = el.scrollHeight;
  }, []);
  const newest = shown[shown.length - 1]?.id ?? 0;
  useLayoutEffect(() => {
    const el = logRef.current;
    if (el && atEnd.current) el.scrollTop = el.scrollHeight;
  }, [open, filter, newest]);

  const onScroll = () => {
    const el = logRef.current;
    if (el) atEnd.current = el.scrollTop + el.clientHeight >= el.scrollHeight - 2;
  };

  const toLines = () => logRef.current?.focus();

  // On the window, so the keys work before the popup has taken focus, and in
  // the capture phase, since the popup stops the arrow keys on their way up.
  useEffect(() => {
    if (!open) return;
    let g = 0;
    const scroll = (to: number | "top" | "end") => {
      const el = logRef.current;
      if (!el) return;
      if (to === "top") el.scrollTop = 0;
      else if (to === "end") el.scrollTop = el.scrollHeight;
      else el.scrollTop = Math.max(0, el.scrollTop + to);
      onScroll();
    };
    const half = () => Math.max(LINE_PX, Math.floor((logRef.current?.clientHeight ?? 0) / 2));
    const onKey = (e: KeyboardEvent) => {
      if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented || isEditable(e.target)) return;
      const pendingG = g > 0 && Date.now() - g < PREFIX_TIMEOUT_MS;
      g = 0;
      switch (e.key) {
        case "j":
        case "ArrowDown":
          e.preventDefault();
          return scroll(LINE_PX);
        case "k":
        case "ArrowUp":
          e.preventDefault();
          return scroll(-LINE_PX);
        case "d":
          e.preventDefault();
          return scroll(half());
        case "u":
          e.preventDefault();
          return scroll(-half());
        case "g":
          e.preventDefault();
          if (pendingG) return scroll("top");
          g = Date.now();
          return;
        case "G":
          e.preventDefault();
          return scroll("end");
        case "/":
          e.preventDefault();
          filterRef.current?.focus();
          filterRef.current?.select();
          return;
        default:
          return;
      }
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [open]);

  const onFilterKey = (e: ReactKeyboardEvent<HTMLInputElement>) => {
    if (e.key === "Enter") {
      e.preventDefault();
      toLines();
      return;
    }
    if (e.key !== "Escape") return;
    e.preventDefault();
    e.stopPropagation();
    if (filter === "") return onOpenChange(false);
    setFilter("");
    toLines();
  };

  const empty = s.activityLog.length === 0;
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent initialFocus={logRef} data-slot="activity-log" className="sm:max-w-3xl">
        <DialogHeader>
          <DialogTitle>Activity log</DialogTitle>
          <DialogDescription>
            What this window reported since it started, oldest first; the newest {ACTIVITY_LOG_CAP} lines.
          </DialogDescription>
        </DialogHeader>
        <div className="grid grid-cols-[auto_1fr] items-center gap-2">
          <label htmlFor={`${id}-filter`} className="text-sm text-muted-foreground">
            Filter
          </label>
          <Input
            id={`${id}-filter`}
            ref={filterRef}
            data-slot="activity-filter"
            value={filter}
            placeholder="Text or level (info, warning, error); / to type"
            autoComplete="off"
            spellCheck={false}
            aria-controls={`${id}-log`}
            onChange={(e) => {
              atEnd.current = true;
              setFilter(e.currentTarget.value);
            }}
            onKeyDown={onFilterKey}
          />
        </div>
        <div
          id={`${id}-log`}
          ref={setLog}
          role="log"
          aria-label="Activity log lines"
          tabIndex={0}
          onScroll={onScroll}
          className="max-h-96 min-h-40 overflow-y-auto rounded-lg border border-border outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
        >
          {shown.length === 0 ? (
            <p className="px-3 py-2 text-sm text-muted-foreground">{empty ? "Nothing reported yet" : "No line matches the filter"}</p>
          ) : (
            <ol className="flex flex-col py-1 font-mono text-xs">
              {shown.map((e) => (
                <li key={e.id} data-log-entry={e.id} data-level={e.level} className="grid grid-cols-[auto_4.5rem_1fr] gap-2 px-3 py-1">
                  <time dateTime={e.at} className="text-muted-foreground tabular-nums">
                    {entryTime(e.at)}
                  </time>
                  <span className={LEVEL_CLASS[e.level]}>{e.level}</span>
                  <span className="break-words whitespace-pre-wrap">{e.text}</span>
                </li>
              ))}
            </ol>
          )}
        </div>
      </DialogContent>
    </Dialog>
  );
}
