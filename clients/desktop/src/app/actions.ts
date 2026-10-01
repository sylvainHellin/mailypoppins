// Runs a GUI action, whichever path asked: a key, the palette, a native menu
// item, or a button. One table, so no path can do something another cannot.

import { useCallback, useRef, type Dispatch, type RefObject } from "react";
import { listWidthFor } from "@/app/layout";
import { createMutations } from "@/app/mutations";
import * as compose from "@/app/compose";
import * as send from "@/app/send";
import * as attachments from "@/app/attachments";
import { openEventSource } from "@/app/calendar";
import { openAgendaRsvp, openRsvp } from "@/app/rsvp";
import { newInvitation } from "@/app/invite";
import { manageSignatures } from "@/app/signatures";
import { composeToContact, copyContactAddress, rebuildContacts, sendVcard, CONTACTS_SEARCH_ID } from "@/app/contacts";
import { cursorRow, discardDialog, retryDialog } from "@/app/outbox";
import { copyFromMessage, openConfig, openLog, openMeta } from "@/app/interop";
import { hiddenNotice, viewPanes } from "@/app/views";
import { saveTheme } from "@/app/theme";
import { saveReaderMode, toggleReaderMode } from "@/app/readerMode";
import { actionTargets, type Action } from "@/app/reducer";
import {
  draftItems,
  draftsShown,
  liveHolds,
  LIST_WIDTH_MIN,
  LIST_WIDTH_STEP,
  readerKey,
  sendingRefusal,
  visibleNotices,
  targetKey,
  type AppState,
  type MessageTarget,
  type MutationDialog,
  type Target,
} from "@/app/state";
import type { ActionId } from "@/keymap/catalog";
import { copyText } from "@/lib/clipboard";
import * as cmd from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";

export const FILTER_INPUT_ID = "mp-list-filter";
export const READER_SCROLL_ID = "mp-reader-scroll";

/** The native menu's item ids (src-tauri/src/menu.rs) and what they run. */
export const MENU_ACTIONS: Record<string, ActionId> = {
  restart_daemon: "restart_daemon",
  toggle_sidebar: "toggle_sidebar",
  widen_list: "widen_list",
  narrow_list: "narrow_list",
  zoom_pane: "toggle_zoom",
  command_palette: "open_palette",
  key_help: "toggle_help",
  keyboard_shortcuts: "toggle_help",
};

const HALF_PAGE = 10;
const PAGE = 20;

/** The list width the Shell draws and the most the measured pane row allows. */
export type ListGeometry = { width: number; max: number };

/**
 * `list` is what the Shell last drew; without it (no Shell mounted) the
 * stored width is taken as drawn, unclamped by any measurement.
 */
export function runAction(id: ActionId, s: AppState, dispatch: Dispatch<Action>, list?: ListGeometry): void {
  switch (id) {
    case "focus_next":
    case "focus_prev": {
      const dir = id === "focus_next" ? 1 : -1;
      dispatch({ type: "cycle_focus", dir });
      // Landing in the reader is an explicit open, as the TUI's Tab into the body pane is.
      const panes = viewPanes(s);
      const pane = panes[(panes.indexOf(s.focus) + dir + panes.length) % panes.length];
      if (pane === "reader") openedRead(s, dispatch);
      return;
    }
    case "focus_sidebar":
      return dispatch({ type: "focus", pane: "sidebar" });
    case "focus_list":
      return dispatch({ type: "focus", pane: "list" });
    case "toggle_help":
      return dispatch({ type: "overlay", overlay: s.overlay === "help" ? null : "help" });
    case "open_palette":
      return dispatch({ type: "overlay", overlay: "palette" });
    case "toggle_zoom":
      return dispatch({ type: "toggle_zoom" });
    case "next_account":
      return dispatch({ type: "next_account" });
    case "next_message":
      return dispatch({ type: "move_selection", to: 1, relative: true });
    case "prev_message":
      return dispatch({ type: "move_selection", to: -1, relative: true });
    case "open_message":
      if (s.selection.message || s.selection.draft || s.selection.hit) dispatch({ type: "focus", pane: "reader" });
      openedRead(s, dispatch);
      return;
    case "select_mailbox":
      return dispatch({ type: "sidebar_enter" });
    case "copy_selector": {
      const selector = s.selection.message?.selector;
      if (!selector) return;
      void copyText(selector, selector, dispatch);
      return;
    }
    case "clear_selection":
      // Marks go first, as the TUI's Esc clears a live selection first; over
      // the outbox view or a full-pane view the marks are hidden, and the
      // view closes instead.
      if (s.marked.keys.size > 0 && !s.outboxView && s.view === "mail") return dispatch({ type: "mark_clear" });
      return dispatch({ type: "clear_selection" });
    case "focus_filter": {
      const focus = () => document.getElementById(FILTER_INPUT_ID)?.focus();
      if (s.outboxView || s.view !== "mail") {
        // The filter belongs to the mailbox list: Mail comes back and the
        // outbox view closes first, as a search closes it, and the field
        // mounts on the next render.
        if (s.view !== "mail") dispatch({ type: "switch_view", view: "mail" });
        if (s.outboxView) dispatch({ type: "close_outbox" });
        setTimeout(focus, 0);
        return;
      }
      dispatch({ type: "focus", pane: "list" });
      queueMicrotask(focus);
      return;
    }
    case "list_top":
      return dispatch({ type: "move_selection", to: "first", relative: false });
    case "list_bottom":
      return dispatch({ type: "move_selection", to: "last", relative: false });
    case "half_page_down":
      return dispatch({ type: "move_selection", to: HALF_PAGE, relative: true });
    case "page_down":
      return dispatch({ type: "move_selection", to: PAGE, relative: true });
    case "toggle_sidebar":
      return dispatch({ type: "toggle_sidebar" });
    // The splitter's keyboard road: a step from the drawn width, within what
    // the pane row leaves the reader; a stored width wider than drawn would
    // otherwise take presses that change nothing on screen.
    case "widen_list":
    case "narrow_list": {
      const { width, max } = list ?? listWidthFor(s.prefs.listWidth, 0);
      const px = width + (id === "widen_list" ? LIST_WIDTH_STEP : -LIST_WIDTH_STEP);
      return dispatch({ type: "set_list_width", px: Math.max(LIST_WIDTH_MIN, Math.min(px, max)) });
    }
    case "restart_daemon":
      return dispatch({ type: "overlay", overlay: "restart" });
    case "back":
      return dispatch({ type: "back" });
    case "search_server": {
      // The typed query, else the one the search shows.
      const query = s.filter.trim() || s.search?.query || "";
      if (!query) {
        dispatch({ type: "notice", text: "Type what to search for, then Shift+Enter searches the server" });
        return runAction("focus_filter", s, dispatch);
      }
      return dispatch({ type: "search_server", query });
    }
    case "cancel_search": {
      const search = s.search;
      if (!search || search.mode !== "server" || search.status !== "running" || !search.operationId) {
        dispatch({ type: "notice", text: "No server search is running" });
        return;
      }
      const id = search.operationId;
      void cmd
        .searchServerCancel(id)
        .then((outcome) => dispatch({ type: "search_server_cancelled", operation_id: id, outcome }))
        .catch((e: unknown) => dispatch({ type: "notice", text: `The cancel failed: ${asGuiError(e).message}` }));
      return;
    }
    case "show_intercepted":
      return dispatch({ type: "overlay", overlay: "intercepted" });
    case "archive":
    case "delete":
    case "move":
    case "toggle_flag":
    case "toggle_read":
      return runMutation(id, actionTargets(s), s, dispatch);
    case "mark_toggle": {
      const t = actionCursor(s);
      if (!t) return;
      dispatch({ type: "mark_toggle", key: targetKey(t) });
      // The TUI's `v` steps to the next row, so a run of `v` marks a run.
      return dispatch({ type: "move_selection", to: 1, relative: true });
    }
    case "mark_range": {
      const t = actionCursor(s);
      if (t) dispatch({ type: "mark_range", key: targetKey(t) });
      return;
    }
    case "mark_all":
      return dispatch({ type: "mark_all" });
    case "mark_clear":
      return dispatch({ type: "mark_clear" });
    case "cancel_hold": {
      // The newest hold still counting, as the TUI's `u` cancels its one hold.
      const live = liveHolds(s);
      if (live.length === 0) {
        dispatch({ type: "notice", text: "No send is being held" });
        return;
      }
      // Every live hold is already being cancelled: the TUI re-queues its
      // cancel and says nothing, and the countdown still shows.
      const hold = live.filter((h) => !h.cancelling).pop();
      if (hold) void createMutations(dispatch).cancelHold(hold.operation_id);
      return;
    }
    case "dismiss_notice": {
      // What the area draws: nothing while `!` hides the notices.
      const shown = visibleNotices(s);
      const newest = shown[shown.length - 1];
      if (newest) dispatch({ type: "dismiss_notice", id: newest.id });
      return;
    }
    case "dismiss_all_notices":
      return dispatch({ type: "dismiss_all_notices" });
    case "toggle_activity": {
      const hidden = !s.prefs.activityHidden;
      dispatch({ type: "toggle_activity_hidden" });
      dispatch({ type: "notice", text: hidden ? "Notices hidden; ! shows them, s l lists them" : "Notices shown" });
      return;
    }
    case "activity_log":
      return dispatch({ type: "overlay", overlay: "activity" });
    case "open_config":
      return void openConfig(dispatch);
    case "open_log":
      return void openLog(dispatch);
    case "copy_sender":
      return copyFromMessage(openMeta(s), "sender", dispatch);
    case "copy_link":
      return copyFromMessage(openMeta(s), "link", dispatch);
    case "copy_subject":
      return copyFromMessage(openMeta(s), "subject", dispatch);
    case "new_draft":
      return compose.newDraft(s, dispatch);
    case "manage_signatures":
      return manageSignatures(s, dispatch);
    case "reply":
    case "reply_all":
      return compose.reply(compose.cursorSource(s), id === "reply_all", dispatch);
    case "forward":
      return compose.forward(compose.cursorSource(s), dispatch);
    case "open_editor":
      // The TUI's `e`: a draft opens in the editor; a received message opens
      // read-only, which in the desktop client is the reader.
      if (draftsShown(s) && s.selection.draft) return void compose.editDraft(s, dispatch);
      if (s.selection.message) return runAction("open_message", s, dispatch, list);
      return;
    case "edit_recipients":
      return void compose.editRecipients(s, dispatch);
    case "approve":
    case "demote":
      return compose.setStatus(s, dispatch, id === "approve");
    case "send":
      return send.sendCursor(s, dispatch);
    case "send_all":
      return send.sendAll(s, dispatch);
    case "open_outbox": {
      const account = s.outboxView?.account ?? s.search?.account ?? s.selection.account;
      if (account) dispatch({ type: "open_outbox", account });
      return;
    }
    case "outbox_retry":
    case "outbox_discard": {
      const view = s.outboxView;
      const row = cursorRow(s);
      if (!view || !row) {
        dispatch({ type: "notice", text: view ? "The outbox has no row to act on" : "Open the outbox first (g o)" });
        return;
      }
      if (s.outboxActions.some((x) => x.account === view.account && x.row_id === row.id)) {
        dispatch({ type: "notice", text: `Row ${row.id} is already being retried or discarded` });
        return;
      }
      const dialog = id === "outbox_retry" ? retryDialog(view.account, row) : discardDialog(view.account, row);
      if (typeof dialog === "string") return dispatch({ type: "notice", text: dialog });
      return dispatch({ type: "open_dialog", dialog });
    }
    case "open_attachment":
      return void attachments.openAttachment(s, dispatch);
    case "save_attachment":
      return void attachments.saveAttachment(s, dispatch);
    case "open_html":
      return void attachments.openHtml(s, dispatch);
    case "attach_file":
      return attachments.attachFile(s, dispatch);
    case "fetch_hit":
      return void attachments.fetchHit(s, dispatch);
    case "view_mail":
      return dispatch({ type: "switch_view", view: "mail" });
    case "view_contacts":
      return dispatch({ type: "switch_view", view: "contacts" });
    case "view_calendar":
      return dispatch({ type: "switch_view", view: "calendar" });
    case "open_settings":
      return dispatch({ type: "switch_view", view: "settings" });
    case "add_account":
      return dispatch({ type: "open_account_wizard" });
    case "theme_dark":
      return void saveTheme(dispatch, "dark");
    case "theme_light":
      return void saveTheme(dispatch, "light");
    case "theme_system":
      return void saveTheme(dispatch, "system");
    case "toggle_reader_mode":
      return void toggleReaderMode(s, dispatch);
    case "reader_html":
      return void saveReaderMode(dispatch, "html");
    case "reader_text":
      return void saveReaderMode(dispatch, "text");
    case "calendar_open_source":
      return void openEventSource(s, dispatch);
    case "calendar_toggle_past":
      return dispatch({ type: "calendar_toggle_past" });
    case "calendar_refresh":
      return dispatch({ type: "calendar_refresh" });
    case "calendar_rsvp":
      return openAgendaRsvp(s, dispatch);
    case "rsvp":
      return void openRsvp(s, dispatch);
    case "new_invitation":
      return newInvitation(s, dispatch);
    case "contacts_search":
      document.getElementById(CONTACTS_SEARCH_ID)?.focus();
      return;
    case "contacts_compose":
      return composeToContact(s, dispatch);
    case "contacts_vcard":
      return void sendVcard(s, dispatch);
    case "contacts_copy":
      return copyContactAddress(s, dispatch);
    case "contacts_rebuild":
      return void rebuildContacts(s, dispatch);
    case "quick_sync":
    case "full_sync": {
      const account = s.search?.account ?? s.selection.account;
      if (!account) return;
      const mode = id === "quick_sync" ? "quick" : "full";
      dispatch({ type: "notice", text: `${mode === "quick" ? "Quick sync" : "Full sync"} of ${account}…` });
      void createMutations(dispatch).sync(account, mode);
      return;
    }
  }
}

// ---------------------------------------------------------------------------
// Mutations: keys, palette, row toggles and the reader toolbar all come here
// ---------------------------------------------------------------------------

export type MutationActionId = "archive" | "delete" | "move" | "toggle_flag" | "toggle_read";

/** The row under the cursor as a target, without the marks. */
function actionCursor(s: AppState): Target | null {
  const account = s.search?.account ?? s.selection.account;
  if (!account) return null;
  if (s.selection.draft && !s.search) return { account, draft: s.selection.draft };
  if (s.selection.message) return { account, row_id: s.selection.message.row_id };
  return null;
}

/** The open message, what the reader toolbar acts on. */
export function openMessageTarget(s: AppState): MessageTarget | null {
  const account = s.selection.account;
  const m = s.selection.message;
  return account && m ? { account, row_id: m.row_id } : null;
}

/**
 * The mark-read an explicit open owes (MSG-08), the TUI's `queue_mark_open_read`:
 * Enter, a double-click on a row, or a focus move into the reader marks an
 * unread received message read through `setRead`, applied at once like the
 * `u` toggle. Moving the cursor marks nothing, and a draft, a row already
 * read (or with a read pending) and a row the model does not show are left
 * alone, so an open marks at most once. The TUI has no delay or setting for it.
 */
export function markOpenRead(t: MessageTarget | null, seen: boolean | null | undefined, dispatch: Dispatch<Action>): void {
  if (!t || seen !== false) return;
  void createMutations(dispatch).setRead([t], true);
}

/** `markOpenRead` for the selected message, as the model shows it. */
function openedRead(s: AppState, dispatch: Dispatch<Action>): void {
  const account = s.search?.account ?? s.selection.account;
  const m = s.selection.message;
  const t = account && m ? { account, row_id: m.row_id } : null;
  markOpenRead(t, t ? flagsOf(s, t)?.seen : null, dispatch);
}

function messageTargets(targets: Target[]): MessageTarget[] {
  return targets.filter((t): t is MessageTarget => "row_id" in t);
}

/** A message's read and flag state, wherever the model shows it. */
export function flagsOf(s: AppState, t: MessageTarget): { seen: boolean; flagged: boolean } | null {
  const list = s.messages.data;
  if (list?.kind === "messages" && list.account === t.account) {
    const row = list.rows.find((r) => r.id === t.row_id);
    if (row) return { seen: row.flags.seen, flagged: row.flags.flagged };
  }
  const hit = s.search?.hits.find((h) => h.account === t.account && h.row_id === t.row_id);
  if (hit) return { seen: hit.flags.seen, flagged: hit.flags.flagged };
  if (s.reader.meta && s.reader.key === readerKey(t.account, t.row_id)) {
    return { seen: s.reader.meta.flags.includes("read"), flagged: s.reader.meta.flags.includes("flagged") };
  }
  return null;
}

/** `from - subject` of one row, as the TUI's confirmation names it. */
function describeTarget(s: AppState, t: Target): string {
  const list = s.messages.data;
  if ("draft" in t) {
    const d = list?.kind === "drafts" ? list.listing.drafts.find((x) => x.id === t.draft) : undefined;
    return `${d?.to || "(no recipient)"} - ${d?.subject || "(no subject)"}`;
  }
  const row =
    (list?.kind === "messages" && list.account === t.account ? list.rows.find((r) => r.id === t.row_id) : undefined) ??
    s.search?.hits.find((h) => h.account === t.account && h.row_id === t.row_id);
  if (row) return `${row.from || "(no sender)"} - ${row.subject || "(no subject)"}`;
  const meta = s.reader.key === readerKey(t.account, t.row_id) ? s.reader.meta : null;
  return `${meta?.from ?? "(no sender)"} - ${meta?.subject ?? "(no subject)"}`;
}

/**
 * The file of each draft target that does not parse. The listing names such a
 * file by its stem, but the daemon's `draft.discard` resolves an id against
 * the drafts that parse and answers -32602 for the stem, and no method takes
 * a path, so the desktop cannot discard it.
 */
function unparseableFiles(s: AppState, targets: Target[]): string[] {
  const list = s.messages.data;
  if (list?.kind !== "drafts") return [];
  const listed = new Set(list.listing.drafts.map((d) => d.id));
  const skipped = new Map(draftItems(list).filter((d) => !listed.has(d.id)).map((d) => [d.id, d.path]));
  return targets.flatMap((t) => {
    const path = "draft" in t && t.account === list.account ? skipped.get(t.draft) : undefined;
    return path ? [path] : [];
  });
}

function emails(n: number): string {
  return `${n} email${n === 1 ? "" : "s"}`;
}

/**
 * Run a mutation over `targets`: archive and delete ask first (the TUI's
 * confirmations), move opens the mailbox picker, flag and read toggle at
 * once. Over a batch, flagging wins when any row is unflagged, and marking
 * read when any is unread (the TUI's rule). `fromMarks` says the targets are
 * the marked rows, which are unmarked once the batch is dispatched, as the
 * TUI clears its selection; a row toggle or the reader toolbar passes false.
 */
export function runMutation(
  id: MutationActionId,
  targets: Target[],
  s: AppState,
  dispatch: Dispatch<Action>,
  fromMarks = s.marked.keys.size > 0,
): void {
  if (targets.length === 0) return;
  const msgs = messageTargets(targets);
  switch (id) {
    case "archive":
    case "delete": {
      const acting = id === "archive" ? msgs : targets;
      if (acting.length === 0) {
        dispatch({ type: "notice", text: "Archive needs a received message; a draft leaves by send or delete" });
        return;
      }
      const broken = id === "delete" ? unparseableFiles(s, acting) : [];
      if (broken.length > 0) {
        const text =
          broken.length === 1
            ? `This file does not parse as a draft; delete ${broken[0]} by hand`
            : `${broken.length} files do not parse as drafts; delete them by hand: ${broken.join(", ")}`;
        dispatch({ type: "notice", text });
        return;
      }
      const busy = sendingRefusal(s, acting);
      if (busy) {
        dispatch({ type: "notice", text: busy });
        return;
      }
      const verb = id === "archive" ? "Archive" : "Delete";
      const single = acting.length === 1 && !fromMarks;
      const dialog: MutationDialog = {
        kind: id,
        targets: acting,
        title: single ? `${verb} this email?` : `${verb} ${emails(acting.length)}?`,
        detail: single ? describeTarget(s, acting[0]) : `${acting.length} selected emails`,
      };
      return dispatch({ type: "open_dialog", dialog });
    }
    case "move": {
      const list = s.messages.data;
      if (!s.search && list?.kind === "drafts") {
        dispatch({ type: "notice", text: "Quick-move is not available in this mailbox" });
        return;
      }
      if (msgs.length === 0) return;
      const account = msgs[0].account;
      const source = s.search ? null : s.selection.mailbox;
      if (moveDestinations(s, account, source).length === 0) {
        dispatch({ type: "notice", text: "No other mailboxes to move to" });
        return;
      }
      return dispatch({ type: "open_dialog", dialog: { kind: "move", targets: msgs, account, source } });
    }
    case "toggle_flag":
    case "toggle_read": {
      if (msgs.length === 0) {
        dispatch({ type: "notice", text: "A draft has no read or flag state" });
        return;
      }
      const m = createMutations(dispatch);
      const flags = msgs.map((t) => flagsOf(s, t));
      if (id === "toggle_flag") void m.setFlag(msgs, flags.some((f) => !f?.flagged));
      else void m.setRead(msgs, flags.some((f) => !f?.seen));
      if (fromMarks) dispatch({ type: "mark_set", keys: targets.map(targetKey), on: false });
      return;
    }
  }
}

/** What the confirmation's OK or the picker's choice runs. */
export function runDialog(dialog: MutationDialog, dispatch: Dispatch<Action>, destination?: string): void {
  if (dialog.kind === "send" || dialog.kind === "send_approved") return send.runSend(dialog, dispatch);
  const m = createMutations(dispatch);
  dispatch({ type: "overlay", overlay: null });
  if (dialog.kind === "outbox_retry" || dialog.kind === "outbox_discard") {
    const run = dialog.kind === "outbox_retry" ? m.retryOutboxRow : m.discardOutboxRow;
    return void run(dialog.account, dialog.row_id);
  }
  if (dialog.kind === "move") {
    if (destination) void m.move(dialog.targets, destination);
  } else if (dialog.kind === "approve" || dialog.kind === "demote") {
    const byAccount = new Map<string, string[]>();
    for (const t of dialog.targets) if ("draft" in t) byAccount.set(t.account, [...(byAccount.get(t.account) ?? []), t.draft]);
    for (const [account, ids] of byAccount) void m.setDraftStatus(account, ids, dialog.kind === "approve");
  } else if (dialog.kind === "archive") {
    void m.archive(messageTargets(dialog.targets));
  } else {
    const msgs = messageTargets(dialog.targets);
    const drafts = dialog.targets.filter((t): t is { account: string; draft: string } => "draft" in t);
    if (msgs.length > 0) void m.remove(msgs);
    const byAccount = new Map<string, string[]>();
    for (const d of drafts) byAccount.set(d.account, [...(byAccount.get(d.account) ?? []), d.draft]);
    for (const [account, ids] of byAccount) void m.discardDrafts(account, ids);
  }
  dispatch({ type: "mark_set", keys: dialog.targets.map(targetKey), on: false });
}

/** The mailboxes a move can go to: the account's, less Drafts and the one the rows are in. */
export function moveDestinations(s: AppState, account: string, source: string | null): { slug: string; label: string }[] {
  const rows =
    s.mailboxes[account]?.data?.mailboxes.map((m) => ({ slug: m.slug, label: m.label, role: m.role })) ??
    s.bootstrap?.snapshot.mailboxes[account]?.map((m) => ({ slug: m.slug, label: m.label, role: m.role })) ??
    [];
  return rows.filter((m) => m.role !== "drafts" && m.slug !== source).map(({ slug, label }) => ({ slug, label }));
}

/**
 * A stable runner bound to the latest state, the palette's and the menu's.
 * Over a full-pane view or the outbox view an action on the hidden mailbox
 * selection answers a notice instead; the keymap drops those keys itself, and the reader's own
 * buttons act on what the reader shows.
 */
export function useRunAction(
  state: AppState,
  dispatch: Dispatch<Action>,
  list?: RefObject<ListGeometry | null>,
): (id: ActionId) => void {
  const ref = useRef(state);
  ref.current = state;
  return useCallback(
    (id: ActionId) => {
      const hidden = hiddenNotice(ref.current, id);
      if (hidden) return dispatch({ type: "notice", text: hidden });
      runAction(id, ref.current, dispatch, list?.current ?? undefined);
    },
    [dispatch, list],
  );
}

export { HALF_PAGE, PAGE };
