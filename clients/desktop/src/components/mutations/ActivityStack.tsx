// The activity area: send holds counting down, then the notices of
// `state.activity`, stacked at the bottom right of the window.
// The applied notices render into one `role="status"` region that is mounted
// before the first of them, since a live region that mounts with its text
// already inside may not be announced; a failure is its own `role="alert"`.
// A hold card's end line is a status region of the card, mounted empty with
// the card, for the same reason. Between the holds and the notices, one card
// per contact rebuild, RSVP and invitation this window awaits, with Cancel.

import { useCallback, useState, type Dispatch } from "react";
import { CircleAlert, CircleCheck, LoaderCircle, Send, Undo2, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { useMutations } from "@/app/mutations";
import { useAppState, useDispatch } from "@/app/store";
import { FAILURES, STICKY_MS } from "@/app/activity";
import { useWindDown } from "@/hooks/use-wind-down";
import { RESPONSE_LABEL } from "@/app/rsvp";
import type { Action } from "@/app/reducer";
import * as cmd from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";
import { visibleNotices, type ActivityNotice, type AppState, type HoldEntry } from "@/app/state";

/** How long an applied notice stays. */
export const APPLIED_MS = 5000;
/** How long a hold that fired or was cancelled shows its end. */
export const HOLD_END_MS = 3000;
/** The failed rows a notice lists before it says how many more. */
const ROWS_SHOWN = 5;

/**
 * Kinds that report a failure: alerts that leave after STICKY_MS or when
 * dismissed (src/app/activity.ts). A hold card's failure or partial delivery
 * leaves after STICKY_MS too.
 */
export { FAILURES, STICKY_MS };

export type NoticeToastProps = { notice: ActivityNotice; onDismiss: (id: number) => void };

/**
 * One notice: an applied batch leaves after APPLIED_MS, a failure after
 * STICKY_MS, either held while the pointer rests on it. A failure is an
 * alert; an applied notice takes no role, the status region it sits in
 * announces it.
 */
export function NoticeToast({ notice, onDismiss }: NoticeToastProps) {
  const failure = FAILURES.has(notice.kind);
  const id = notice.id;
  const done = useCallback(() => onDismiss(id), [id, onDismiss]);
  const hover = useWindDown(failure ? STICKY_MS : APPLIED_MS, done);
  const extra = notice.rows.length - ROWS_SHOWN;
  return (
    <div
      role={failure ? "alert" : undefined}
      data-notice={notice.kind}
      {...hover}
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
  /**
   * A send of this window awaits the hold's operation: after the fire the
   * card says "Sending…" until the outcome arrives, where a hold another
   * client armed says "Sent" at the fire.
   */
  awaiting?: boolean;
  onCancel: (operationId: string) => void;
  onGone: (operationId: string) => void;
};

/**
 * What a hold's card says once the countdown is over, or null while it counts.
 */
export function holdEndText(hold: HoldEntry, awaiting: boolean): string | null {
  if (hold.outcome) return hold.outcome.text;
  if (hold.state === "cancelled") return "Send cancelled";
  if (hold.state === "fired") return awaiting ? "Sending…" : "Sent";
  return null;
}

/**
 * A send waiting out its undo window. The seconds are the daemon's, from
 * the last `send.hold_tick`, never a local clock. Its end ("Sent", "Send
 * cancelled", "Failed: …", "Partly delivered: …") goes into a status
 * region the card mounts empty; the card then leaves after HOLD_END_MS, or,
 * for a failure or a partial delivery of one draft, after STICKY_MS or when
 * dismissed, held while the pointer rests on it.
 */
export function HoldToast({ hold, awaiting = false, onCancel, onGone }: HoldToastProps) {
  const counting = hold.state === "started" || hold.state === "tick";
  const end = holdEndText(hold, awaiting);
  const settled = hold.outcome !== undefined || hold.state === "cancelled" || (hold.state === "fired" && !awaiting);
  const sticky = hold.outcome?.sticky ?? false;
  const operationId = hold.operation_id;
  const gone = useCallback(() => onGone(operationId), [operationId, onGone]);
  const hover = useWindDown(settled ? (sticky ? STICKY_MS : HOLD_END_MS) : null, gone);
  const subject = hold.subject || "(no subject)";
  const secs = Math.max(0, Math.round(hold.remaining_secs));
  const share = hold.hold_secs > 0 ? Math.min(100, (100 * secs) / hold.hold_secs) : 0;
  const tone = hold.outcome?.tone;
  const icon =
    tone === "failed" || tone === "partial" ? (
      <CircleAlert aria-hidden="true" className="size-4 shrink-0 text-destructive" />
    ) : (
      <Send aria-hidden="true" className="size-4 shrink-0 text-warning" />
    );
  return (
    <div
      role="group"
      aria-label={`Held send: ${subject}`}
      data-hold={hold.operation_id}
      data-state={hold.state}
      data-outcome={tone}
      {...hover}
      className="flex flex-col gap-1.5 rounded-lg border border-border bg-popover px-3 py-2 text-sm text-popover-foreground shadow-md"
    >
      <div className="flex items-center gap-2">
        {icon}
        <div className="flex min-w-0 flex-1 flex-col">
          {counting ? <p className="tabular-nums">{`Sending in ${secs} s`}</p> : null}
          <p role="status" data-slot="hold-end" className="break-words">
            {end ?? ""}
          </p>
          <p className="truncate text-xs text-muted-foreground" title={subject}>
            {`${subject}, from ${hold.account}`}
          </p>
        </div>
        {sticky ? (
          <Button size="icon-xs" variant="ghost" aria-label="Dismiss" onClick={() => onGone(hold.operation_id)}>
            <X aria-hidden="true" />
          </Button>
        ) : null}
        {!counting ? null : (
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
      {!counting ? null : (
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

/** An operation this window awaits that a card offers to cancel. */
export type RunningOperation = { operation_id: string; kind: "contact_rebuild" | "rsvp" | "send_invite"; text: string; cancelLabel: string };

/**
 * The contact rebuilds, RSVPs and invitations this window awaits, in that
 * order, once their start answered with an id: before that there is nothing
 * to name in a cancel.
 */
export function runningOperations(s: AppState): RunningOperation[] {
  const out: RunningOperation[] = [];
  for (const r of s.rebuilds) {
    if (!r.operation_id) continue;
    out.push({ operation_id: r.operation_id, kind: "contact_rebuild", text: `Rebuilding the contact index of ${r.account}…`, cancelLabel: "Cancel the contact rebuild" });
  }
  for (const r of s.rsvps) {
    if (!r.operation_id) continue;
    out.push({ operation_id: r.operation_id, kind: "rsvp", text: `Sending ${RESPONSE_LABEL[r.response]} to ${r.summary}…`, cancelLabel: "Cancel the RSVP" });
  }
  for (const r of s.inviteSends) {
    if (!r.operation_id) continue;
    out.push({ operation_id: r.operation_id, kind: "send_invite", text: `Sending the invitation ${r.subject}…`, cancelLabel: "Cancel the invitation" });
  }
  return out;
}

/**
 * Cancel one operation by id (`operation_cancel`). It stays awaited, so its
 * `cancelled` finish ends it and says so; a cancel that came too late
 * answers `already_settled` and its own end says how it went. Resolves
 * false when the cancel itself failed, which a notice says.
 */
export async function cancelOperation(dispatch: Dispatch<Action>, operationId: string): Promise<boolean> {
  try {
    await cmd.operationCancel(operationId);
    return true;
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `The cancel failed: ${asGuiError(e).message}`, level: "error" });
    return false;
  }
}

/**
 * One running operation with its Cancel, disabled while the cancel is in
 * flight and until the operation's end takes the card away.
 */
export function OperationToast({ op }: { op: RunningOperation }) {
  const dispatch = useDispatch();
  const [cancelling, setCancelling] = useState(false);
  return (
    <div
      role="group"
      aria-label={op.text}
      data-operation={op.operation_id}
      data-kind={op.kind}
      className="flex items-center gap-2 rounded-lg border border-border bg-popover px-3 py-2 text-sm text-popover-foreground shadow-md"
    >
      <LoaderCircle aria-hidden="true" className="size-4 shrink-0 animate-spin text-muted-foreground" />
      <p className="min-w-0 flex-1 break-words">{op.text}</p>
      <Button
        size="xs"
        variant="outline"
        aria-label={op.cancelLabel}
        disabled={cancelling}
        onClick={() => {
          setCancelling(true);
          void cancelOperation(dispatch, op.operation_id).then((ok) => {
            if (!ok) setCancelling(false);
          });
        }}
      >
        <Undo2 aria-hidden="true" />
        {cancelling ? "Cancelling…" : "Cancel"}
      </Button>
    </div>
  );
}

/**
 * The activity area. A cancelled hold's notice stays in the model and is not
 * shown, since the hold's own toast says so. While `!` hides the notices the
 * hold cards still show, so a send can always be cancelled. The area stays
 * mounted when empty, so its status region exists before the first notice.
 */
export function ActivityStack() {
  const s = useAppState();
  const dispatch = useDispatch();
  const m = useMutations();
  const holds = Object.values(s.holds);
  const running = runningOperations(s);
  const awaited = new Set(s.sends.flatMap((r) => (r.operation_id ? [r.operation_id] : [])));
  // `!` hides the notices (prefs.activityHidden); a hold card always shows.
  const notices = visibleNotices(s);
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
          <HoldToast key={h.operation_id} hold={h} awaiting={awaited.has(h.operation_id)} onCancel={onCancel} onGone={onGone} />
        ))}
      </div>
      <div className="flex flex-col gap-2">
        {running.map((op) => (
          <OperationToast key={op.operation_id} op={op} />
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
