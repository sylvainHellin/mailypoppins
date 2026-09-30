import { useId } from "react";
import { Check, CircleHelp, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Skeleton } from "@/components/ui/skeleton";
import { EventCard } from "@/components/calendar/EventCard";
import { isSentMailbox, RESPONSE_LABEL, rsvpOf, rsvpRefusal, sendRsvp } from "@/app/rsvp";
import { useAppState, useDispatch } from "@/app/store";
import { readerKey, RSVP_RESPONSES, type RsvpResponse } from "@/app/state";
import type { MessageMeta } from "@/lib/gui-types";

const ICON: Record<RsvpResponse, typeof Check> = { accept: Check, tentative: CircleHelp, decline: X };

/**
 * The invitation an email carries, under the reader's header
 * (clients/desktop/docs/reader.md, "Invitations"): the shared event card and
 * the three replies. A reply the TUI would refuse is disabled, with the
 * TUI's sentence below the buttons; a Graph account shows the daemon's.
 */
export function InviteCard({ meta }: { meta: MessageMeta }) {
  const s = useAppState();
  const dispatch = useDispatch();
  const reasonId = useId();
  const l = s.invites[readerKey(meta.account, meta.row_id)];
  if (!l || (l.loadedGen === 0 && !l.error)) {
    return (
      <div aria-busy="true" aria-label="Loading the invitation" className="flex flex-col gap-2 border-b border-border px-5 py-4">
        <Skeleton className="h-5 w-1/2" />
        <Skeleton className="h-4 w-1/3" />
      </div>
    );
  }
  if (l.data === null) {
    return (
      <p role={l.error ? "alert" : undefined} data-slot="invite-card-empty" className="border-b border-border px-5 py-3 text-sm text-muted-foreground">
        {l.error ? `The invitation did not load: ${l.error.message}` : "This email carries no invitation the store could read."}
      </p>
    );
  }
  const event = l.data;
  const organizer = isSentMailbox(s, meta.account, meta.mailbox);
  const refusal = rsvpRefusal(event, organizer, s.inviteRefusals[meta.account] ?? null);
  const running = rsvpOf(s, meta.account, meta.row_id);
  const summary = event.summary?.trim() || meta.subject || "(no subject)";
  const target = { account: meta.account, row_id: meta.row_id, summary };
  return (
    <div data-slot="invite-card" className="border-b border-border px-5 py-4">
      <EventCard event={event} organizer={organizer} label="Invitation">
        <div role="group" aria-label="Reply to the invitation" className="flex flex-wrap items-center gap-1.5 pt-1">
          {RSVP_RESPONSES.map((response) => {
            const Icon = ICON[response];
            return (
              <Button
                key={response}
                size="sm"
                variant="outline"
                disabled={refusal !== null || running !== null}
                aria-describedby={refusal ? reasonId : undefined}
                data-response={response}
                onClick={() => void sendRsvp(dispatch, target, response)}
              >
                <Icon aria-hidden="true" />
                {RESPONSE_LABEL[response]}
              </Button>
            );
          })}
        </div>
        {refusal ? (
          <p id={reasonId} data-slot="rsvp-refusal" className="text-xs text-muted-foreground">
            {refusal}
          </p>
        ) : running ? (
          <p data-slot="rsvp-sending" className="text-xs text-muted-foreground" aria-live="polite">
            Sending {RESPONSE_LABEL[running.response]}…
          </p>
        ) : (
          <p className="text-xs text-muted-foreground">t v opens the reply from the keyboard.</p>
        )}
      </EventCard>
    </div>
  );
}
