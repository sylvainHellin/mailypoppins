import { TriangleAlert } from "lucide-react";
import { SidebarMenuItem } from "@/components/ui/sidebar";
import { givenUpHeadline, givenUpSentence } from "@/app/givenUp";
import type { GivenUpMessage } from "@/protocol/types";

/**
 * The account's messages the sync has given up on (PERSO-334), under its
 * mailboxes: a warning line that opens onto one sentence per message, in the
 * server's words, with when mp asks again. Nothing when there are none.
 */
export function GivenUpState({ account, givenUp }: { account: string; givenUp: GivenUpMessage[] }) {
  if (givenUp.length === 0) return null;
  const count = givenUp.length;
  const now = new Date();
  return (
    <SidebarMenuItem data-given-up={account}>
      <details className="px-2 py-1 text-xs text-warning">
        <summary className="flex cursor-pointer items-center gap-2">
          <TriangleAlert aria-hidden="true" className="size-4 shrink-0" />
          <span className="truncate">{count === 1 ? "1 message not downloaded" : `${count} messages not downloaded`}</span>
        </summary>
        <p className="mt-1 text-muted-foreground">{givenUpHeadline(count)}.</p>
        <ul className="mt-1 space-y-1 text-muted-foreground">
          {givenUp.map((m) => (
            <li key={`${m.mailbox}/${m.uid}`}>{givenUpSentence(m, now)}.</li>
          ))}
        </ul>
      </details>
    </SidebarMenuItem>
  );
}
