// The activity area: send holds counting down, then the notices of
// `state.activity`, stacked at the bottom right of the window.
// The applied notices render into one `role="status"` region that is mounted
// before the first of them, since a live region that mounts with its text
// already inside may not be announced; a failure is its own `role="alert"`.

import { useCallback, useEffect } from "react";
import { CircleAlert, CircleCheck, Send, Undo2, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useMutations } from "@/app/mutations";
import { useAppState, useDispatch } from "@/app/store";
import { shownNotices, type ActivityNotice, type HoldEntry } from "@/app/state";

/** How long an applied notice stays. */
export const APPLIED_MS = 5000;
/** How long a hold that fired or was cancelled shows its end. */
export const HOLD_END_MS = 3000;
/** The failed rows a notice lists before it says how many more. */
const ROWS_SHOWN = 5;

/** Kinds that report a failure: they stay until dismissed and are alerts. */
export const FAILURES: ReadonlySet<ActivityNotice["kind"]> = new Set([
  "failed",
  "rolled_back",
  "hold_cancel_failed",
  "sync_failed",
  "compose_failed",
]);

export type NoticeToastProps = { notice: ActivityNotice; onDismiss: (id: number) => void };

/**
 * One notice: an applied batch leaves by itself, a failure stays. A failure
 * is an alert; an applied notice takes no role, the status region it sits in
 * announces it.
 */
export function NoticeToast({ notice, onDismiss }: NoticeToastProps) {
  const failure = FAILURES.has(notice.kind);
  useEffect(() => {
    if (failure) return;
    const t = setTimeout(() => onDismiss(notice.id), APPLIED_MS);
    return () => clearTimeout(t);
  }, [failure, notice.id, onDismiss]);
  const extra = notice.rows.length - ROWS_SHOWN;
  return (
    <div
      role={failure ? "alert" : undefined}
      data-notice={notice.kind}
      className="flex items-start gap-2 rounded-lg border border-border bg-popover px-3 py-2 text-sm text-popover-foreground shadow-md"
    >
      {failure ? (
        <CircleAlert aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-destructive" />
      ) : (
        <CircleCheck aria-hidden="true" className="mt-0.5 size-4 shrink-0 text-link" />
      )}
      <div className="flex min-w-0 flex-1 flex-col gap-1">
        <p className="break-words">{notice.text}</p>
        {notice.rows.length > 0 ? (
          <ul className="flex flex-col gap-0.5 text-xs text-muted-foreground">
            {notice.rows.slice(0, ROWS_SHOWN).map((r) => (
              <li key={r.key} className="break-words">
                {`${r.label}: ${r.reason}`}
              </li>
            ))}
            {extra > 0 ? <li>{`and ${extra} more`}</li> : null}
          </ul>
        ) : null}
      </div>
      <Button size="icon-xs" variant="ghost" aria-label="Dismiss" onClick={() => onDismiss(notice.id)}>
        <X aria-hidden="true" />
      </Button>
    </div>
  );
}

export type HoldToastProps = {
  hold: HoldEntry;
  onCancel: (operationId: string) => void;
  onGone: (operationId: string) => void;
};

/**
 * A send waiting out its undo window. The seconds are the daemon's, from
 * the last `send.hold_tick`, never a local clock. A hold that fired or was
 * cancelled says so for a moment and leaves.
 */
export function HoldToast({ hold, onCancel, onGone }: HoldToastProps) {
  const over = hold.state === "fired" || hold.state === "cancelled";
  useEffect(() => {
    if (!over) return;
    const t = setTimeout(() => onGone(hold.operation_id), HOLD_END_MS);
    return () => clearTimeout(t);
  }, [over, hold.operation_id, onGone]);
  const subject = hold.subject || "(no subject)";
  const secs = Math.max(0, Math.round(hold.remaining_secs));
  const share = hold.hold_secs > 0 ? Math.min(100, (100 * secs) / hold.hold_secs) : 0;
  return (
    <div
      role="group"
      aria-label={`Held send: ${subject}`}
      data-hold={hold.operation_id}
      data-state={hold.state}
      className="flex flex-col gap-1.5 rounded-lg border border-border bg-popover px-3 py-2 text-sm text-popover-foreground shadow-md"
    >
      <div className="flex items-center gap-2">
        <Send aria-hidden="true" className="size-4 shrink-0 text-warning" />
        <div className="flex min-w-0 flex-1 flex-col">
          {over ? (
            <p role="status">{hold.state === "fired" ? "Sent" : "Send cancelled"}</p>
          ) : (
            <p className="tabular-nums">{`Sending in ${secs} s`}</p>
          )}
          <p className="truncate text-xs text-muted-foreground" title={subject}>
            {`${subject}, from ${hold.account}`}
          </p>
        </div>
        {over ? null : (
          <Button
            size="xs"
            variant="outline"
            aria-label="Cancel send"
            title="Cancel send (u)"
            disabled={hold.cancelling}
            onClick={() => onCancel(hold.operation_id)}
          >
            <Undo2 aria-hidden="true" />
            {hold.cancelling ? "Cancelling…" : "Cancel"}
          </Button>
        )}
      </div>
      {over ? null : (
        <div
          role="progressbar"
          aria-label="Time left before the send"
          aria-valuemin={0}
          aria-valuemax={hold.hold_secs}
          aria-valuenow={secs}
          aria-valuetext={`${secs} seconds left`}
          className="h-1 overflow-hidden rounded-full bg-muted"
        >
          <div className="h-full bg-warning transition-[width] duration-1000 ease-linear" style={{ width: `${share}%` }} />
        </div>
      )}
    </div>
  );
}

/**
 * The activity area. A cancelled hold's notice stays in the model and is not
 * shown, since the hold's own toast says so. The area stays mounted when
 * empty, so its status region exists before the first notice.
 */
export function ActivityStack() {
  const s = useAppState();
  const dispatch = useDispatch();
  const m = useMutations();
  const holds = Object.values(s.holds);
  const notices = shownNotices(s);
  const applied = notices.filter((n) => !FAILURES.has(n.kind));
  const failures = notices.filter((n) => FAILURES.has(n.kind));
  // Stable, so a hold's tick does not restart a notice's timer.
  const onDismiss = useCallback((id: number) => dispatch({ type: "dismiss_notice", id }), [dispatch]);
  const onGone = useCallback((operation_id: string) => dispatch({ type: "dismiss_hold", operation_id }), [dispatch]);
  const onCancel = useCallback((operation_id: string) => void m.cancelHold(operation_id), [m]);
  return (
    <section
      aria-label="Activity"
      className={`fixed right-4 z-40 flex max-h-[70vh] w-80 max-w-[calc(100vw-2rem)] flex-col overflow-y-auto [&>:not(:empty)~:not(:empty)]:mt-2 ${
        s.interceptNotice ? "bottom-14" : "bottom-4"
      }`}
    >
      <div className="flex flex-col gap-2">
        {holds.map((h) => (
          <HoldToast key={h.operation_id} hold={h} onCancel={onCancel} onGone={onGone} />
        ))}
      </div>
      <div className="flex flex-col gap-2">
        {failures.map((n) => (
          <NoticeToast key={n.id} notice={n} onDismiss={onDismiss} />
        ))}
      </div>
      <div role="status" data-slot="activity-status" className="flex flex-col gap-2">
        {applied.map((n) => (
          <NoticeToast key={n.id} notice={n} onDismiss={onDismiss} />
        ))}
      </div>
    </section>
  );
}
