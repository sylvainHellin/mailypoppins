// The blocked senders (PERSO-261): the TUI's `B` chooser and its `sb` list.
// The daemon keeps one list for every account; a block moves the sender's
// mail in each account's Inbox to that account's Junk folder, now and after
// every sync, and an unblock only removes the entry. The list in the model
// is the open dialog's: read when it opens and on every bootstrap, and
// replaced whole by every `sender.blocklist_changed`, which names the whole
// list, and by every block or unblock answer, which does too.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";
import { draftsShown, expandedMember, listedRow, markStale, readerKey, type AppState, type BlockDialog } from "@/app/state";
import * as cmd from "@/lib/commands";
import { asGuiError, type GuiError } from "@/lib/gui-types";
import { comparableAddress, senderAddress } from "@/lib/mailbox";
import type { SenderBlocked } from "@/protocol/types";

/**
 * The chooser for a message whose `From` is `from`: the address a reply goes
 * to as the list would hold it, and its `@domain`; null when it names no
 * address with a domain (`mp-tui`'s `BlockChooser::for_sender`).
 */
export function blockChoice(from: string): BlockDialog | null {
  const address = comparableAddress(senderAddress(from).trim());
  const at = address.lastIndexOf("@");
  if (at <= 0) return null;
  const domain = address.slice(at + 1);
  if (domain === "" || /\s/.test(domain)) return null;
  return { address, domain: `@${domain}` };
}

/** The cursor message's `From`, wherever the model shows it, or why there is nothing to block. */
function cursorFrom(s: AppState): { from: string } | { refusal: string } {
  const account = s.search?.account ?? s.selection.account;
  const m = s.selection.message;
  if ((draftsShown(s) && s.selection.draft) || (!m && s.selection.draft)) return { refusal: "Block needs a received message" };
  if (!account || !m) return { refusal: "Select a message first" };
  const list = s.messages.data;
  const from =
    listedRow(list, account, m.row_id)?.from ??
    expandedMember(s, account, m.row_id)?.from ??
    s.search?.hits.find((h) => h.account === account && h.row_id === m.row_id)?.from ??
    (s.reader.key === readerKey(account, m.row_id) ? s.reader.meta?.from : null);
  return from ? { from } : { refusal: "This message has no sender" };
}

/** `B` and the palette's row: the chooser on the cursor message's sender, or a notice that says why not. */
export function openBlockSender(s: AppState, dispatch: Dispatch<Action>): void {
  const found = cursorFrom(s);
  if ("refusal" in found) return dispatch({ type: "notice", text: found.refusal });
  const dialog = blockChoice(found.from);
  if (!dialog) return dispatch({ type: "notice", text: `No address to block in '${found.from}'` });
  dispatch({ type: "open_block_sender", dialog });
}

/**
 * What a block came to, the TUI's status line: the entry, how many messages
 * moved to Junk and on which accounts, and the daemon's sentence for each
 * account with no Junk folder (`mp-tui`'s `block_summary`).
 */
export function blockSummary(blocked: SenderBlocked): string {
  let out = blocked.added ? `Blocked ${blocked.entry}` : `${blocked.entry} was already blocked`;
  const moved = blocked.moved.reduce((n, m) => n + m.count, 0);
  if (moved > 0) {
    const accounts = blocked.moved.map((m) => `${m.account} ${m.count}`).join(", ");
    out += `; ${moved} ${moved === 1 ? "message" : "messages"} moved to Junk (${accounts})`;
  } else {
    out += "; no Inbox mail from it";
  }
  for (const skipped of blocked.skipped) out += `; ${skipped.reason}`;
  return out;
}

/** What a block or an unblock came to: `ok`, or the sentence the dialog shows. */
export type SenderOutcome = { ok: true } | { ok: false; error: string };

/** Enter on the chooser: `sender_block` on `entry`, the summary as a notice, a warning when an account was left alone. */
export async function blockSender(dispatch: Dispatch<Action>, entry: string): Promise<SenderOutcome> {
  try {
    const blocked = await cmd.senderBlock(entry);
    dispatch({ type: "blocked_senders_set", entries: blocked.entries });
    dispatch({ type: "notice", text: blockSummary(blocked), level: blocked.skipped.length > 0 ? "warning" : "info" });
    return { ok: true };
  } catch (e: unknown) {
    return { ok: false, error: `Cannot block ${entry}: ${asGuiError(e).message}` };
  }
}

/** `u` on the Blocked senders dialog: `sender_unblock` on `entry`. */
export async function unblockSender(dispatch: Dispatch<Action>, entry: string): Promise<SenderOutcome> {
  try {
    const unblocked = await cmd.senderUnblock(entry);
    dispatch({ type: "blocked_senders_set", entries: unblocked.entries });
    const text = unblocked.removed ? `Unblocked ${unblocked.entry}; its mail already in Junk stays there` : `${unblocked.entry} was not blocked`;
    dispatch({ type: "notice", text });
    return { ok: true };
  } catch (e: unknown) {
    return { ok: false, error: `Cannot unblock ${entry}: ${asGuiError(e).message}` };
  }
}

/** The Blocked senders dialog opens on a list read again. */
export function openBlockedSenders(s: AppState): AppState {
  return { ...s, overlay: "blocked_senders", blockedSenders: markStale(s.blockedSenders), blockDialog: null };
}

/** A `sender_list` answer, kept only when nothing replaced the list since it was asked. */
export function blockedSendersLoaded(s: AppState, gen: number, entries: string[]): AppState {
  if (gen !== s.blockedSenders.gen) return s;
  return { ...s, blockedSenders: { ...s.blockedSenders, data: entries, loadedGen: gen, error: null } };
}

export function blockedSendersFailed(s: AppState, gen: number, error: GuiError): AppState {
  if (gen !== s.blockedSenders.gen) return s;
  return { ...s, blockedSenders: { ...s.blockedSenders, loadedGen: gen, error } };
}

/**
 * The whole list, from an event or an answer: the truth, so a read still
 * under way, asked before it, is dropped when it lands.
 */
export function blockedSendersReplaced(s: AppState, entries: string[]): AppState {
  const gen = s.blockedSenders.gen + 1;
  return { ...s, blockedSenders: { data: entries, gen, loadedGen: gen, error: null } };
}

/** Where the cursor goes when `entry` leaves `entries`: the next row, else the one before. */
export function cursorAfterUnblock(entries: readonly string[], entry: string): string | null {
  const at = entries.indexOf(entry);
  if (at < 0) return entries[0] ?? null;
  return entries[at + 1] ?? entries[at - 1] ?? null;
}
