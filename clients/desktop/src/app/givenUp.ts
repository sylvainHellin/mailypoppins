// The messages the sync has given up on for now (PERSO-334): listed by the
// server, never stored, asked for again later. The list is the bootstrap's
// `sync_health.given_up`, replaced by every `sync.completed` that ran; the
// wording is `mp_client::format::given_up_sentence` and `given_up_headline`,
// which `mp sync` prints.
import type { AppState } from "@/app/state";
import type { GivenUpMessage } from "@/protocol/types";

/** The account's messages given up on, empty when it has none. */
export function givenUpOf(s: AppState, account: string): GivenUpMessage[] {
  return s.bootstrap?.snapshot.accounts.find((a) => a.name === account)?.sync_health.given_up ?? [];
}

/**
 * The state with the account's list replaced, as a `sync.completed` that ran
 * carries it. Held in the bootstrap's snapshot, where a fresh bootstrap puts
 * the daemon's own copy, so the two never disagree.
 */
export function withGivenUp(s: AppState, account: string, givenUp: GivenUpMessage[]): AppState {
  if (!s.bootstrap) return s;
  return {
    ...s,
    bootstrap: {
      ...s.bootstrap,
      snapshot: {
        ...s.bootstrap.snapshot,
        accounts: s.bootstrap.snapshot.accounts.map((a) =>
          a.name === account ? { ...a, sync_health: { ...a.sync_health, given_up: givenUp } } : a,
        ),
      },
    },
  };
}

/** How many messages could not be downloaded, and that the server still holds them. */
export function givenUpHeadline(count: number): string {
  return count === 1
    ? "1 message could not be downloaded; it is still on the server, nothing is lost"
    : `${count} messages could not be downloaded; they are still on the server, nothing is lost`;
}

/** `YYYY-MM-DD HH:MM` in local time. */
function localMinute(at: Date): string {
  const two = (n: number) => String(n).padStart(2, "0");
  return `${at.getFullYear()}-${two(at.getMonth() + 1)}-${two(at.getDate())} ${two(at.getHours())}:${two(at.getMinutes())}`;
}

/**
 * One message as a sentence for a reader who never saw the code: which
 * message (its mailbox and UID are all mp knows of it), why it is not here,
 * in the server's words for a refusal, and when mp asks again.
 */
export function givenUpSentence(m: GivenUpMessage, now: Date = new Date()): string {
  const why = m.kind === "refused" ? `the server refuses to send it (${m.reason})` : `mp could not store it (${m.reason})`;
  const at = m.retry_at ? new Date(m.retry_at) : null;
  const retry =
    at === null || Number.isNaN(at.getTime())
      ? "mp asks again on every sync"
      : at > now
        ? `mp asks again after ${localMinute(at)}`
        : "mp asks again on the next sync";
  return `${m.mailbox}, UID ${m.uid} (mp knows it only by its mailbox and number): ${why}; ${retry}`;
}
