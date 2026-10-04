// The GUI's view of the TUI KEYMAP: src/keymap/keymap.json is generated from
// `mp dump-keys --json` (pnpm gen:keymap), and the key help and the command
// palette both render from it. What this file adds is only the binding from
// a KEYMAP row to a GUI action, or the milestone that will bring it.

import keymap from "@/keymap/keymap.json";

export type KeymapBinding = { key: string; action: string };
export type KeymapSection = { title: string; bindings: KeymapBinding[] };

export const SECTIONS: KeymapSection[] = keymap;

/** What the shell can run. */
export type ActionId =
  | "focus_next"
  | "focus_prev"
  | "focus_sidebar"
  | "focus_list"
  | "focus_reader"
  | "toggle_help"
  | "toggle_zoom"
  | "open_palette"
  | "next_account"
  | "next_message"
  | "prev_message"
  | "open_message"
  | "select_mailbox"
  | "copy_selector"
  | "clear_selection"
  | "focus_filter"
  | "list_top"
  | "list_bottom"
  | "half_page_down"
  | "page_down"
  | "toggle_sidebar"
  | "widen_list"
  | "narrow_list"
  | "restart_daemon"
  | "back"
  | "search_server"
  | "cancel_search"
  | "show_intercepted"
  | "archive"
  | "delete"
  | "move"
  | "toggle_flag"
  | "toggle_read"
  | "mark_toggle"
  | "mark_range"
  | "mark_all"
  | "mark_clear"
  | "cancel_hold"
  | "dismiss_notice"
  | "dismiss_all_notices"
  | "quick_sync"
  | "full_sync"
  | "new_draft"
  | "manage_signatures"
  | "reply"
  | "reply_all"
  | "forward"
  | "open_editor"
  | "edit_recipients"
  | "approve"
  | "demote"
  | "send"
  | "send_all"
  | "open_outbox"
  | "outbox_retry"
  | "outbox_discard"
  | "open_attachment"
  | "save_attachment"
  | "open_html"
  | "attach_file"
  | "fetch_hit"
  | "view_mail"
  | "view_contacts"
  | "view_calendar"
  | "open_settings"
  | "add_account"
  | "calendar_open_source"
  | "calendar_toggle_past"
  | "calendar_refresh"
  | "calendar_rsvp"
  | "rsvp"
  | "new_invitation"
  | "contacts_search"
  | "contacts_compose"
  | "contacts_vcard"
  | "contacts_copy"
  | "contacts_rebuild"
  | "toggle_activity"
  | "activity_log"
  | "open_config"
  | "open_log"
  | "copy_sender"
  | "copy_link"
  | "copy_subject"
  | "theme_dark"
  | "theme_light"
  | "theme_system"
  | "toggle_reader_mode"
  | "reader_html"
  | "reader_text";

/**
 * Why a KEYMAP row cannot run from the palette in this build:
 * a later milestone of the plan, the next M1 unit ("soon"), a row that only
 * makes sense as a key ("key", e.g. j/k or 1-9), or one the app menu owns.
 */
export type Badge = "M3" | "M4" | "soon" | "key" | "menu" | "later";

/**
 * `keys` replaces the KEYMAP's where the desktop binds the action to another
 * key, or to none where the desktop gives the TUI's key to another action.
 */
type Binding = { id: ActionId; keys?: string[] } | { badge: Badge; keys?: string[] };

/** By the KEYMAP description, whatever the section. */
const BY_ACTION: Record<string, Binding> = {
  Quit: { badge: "menu" },
  "Jump to mailbox": { badge: "key" },
  "Cycle focus forward": { id: "focus_next" },
  "Cycle focus backward": { id: "focus_prev" },
  "Toggle this help": { id: "toggle_help" },
  "Command palette (run an action by name)": { badge: "key" },
  "Zoom / unzoom the focused pane": { id: "toggle_zoom" },
  // The desktop has no bottom log pane: `!` hides or shows the activity
  // area's notices, never a live hold card (D12).
  "Toggle activity log": { id: "toggle_activity" },
  "Send current draft (approve + send)": { id: "send" },
  "Search all mail (sender, subject, body)": { id: "search_server" },
  "New draft": { id: "new_draft" },
  "Manage signatures": { id: "manage_signatures" },
  "Go to mailboxes (sidebar)": { id: "focus_sidebar" },
  "Switch account": { id: "next_account" },
  "Quick sync": { id: "quick_sync" },
  "Full sync": { id: "full_sync" },
  "Activity log overlay": { id: "activity_log" },
  "Open config.toml in $EDITOR": { id: "open_config" },
  "Open log file in $EDITOR": { id: "open_log" },
  "Switch to Mail view": { id: "view_mail" },
  "Switch to Contacts view": { id: "view_contacts" },
  "Switch to Calendar view": { id: "view_calendar" },
  "Next / previous message": { id: "next_message" },
  "Open in editor (mail read-only)": { id: "open_editor" },
  Reply: { id: "reply" },
  "Reply all": { id: "reply_all" },
  Forward: { id: "forward" },
  Archive: { id: "archive" },
  Delete: { id: "delete" },
  "Toggle read/unread": { id: "toggle_read" },
  "Toggle flag/star": { id: "toggle_flag" },
  "Move to mailbox (fuzzy picker)": { id: "move" },
  "Copy selector (mp://)": { id: "copy_selector" },
  "Clear selection / return to list": { id: "clear_selection" },
  "Filter the current list": { id: "focus_filter" },
  // The desktop has no thread view yet, and its `tt` toggles the reader's
  // text mode (GUI_ENTRIES), so the row lists no key.
  "Show conversation (thread)": { badge: "later", keys: [] },
  "Open attachment": { id: "open_attachment" },
  "Save attachment to disk": { id: "save_attachment" },
  "Open HTML in browser": { id: "open_html" },
  "RSVP to invitation (Accept/Tentative/Decline)": { id: "rsvp" },
  "Navigate mailboxes": { badge: "key" },
  "Select mailbox": { id: "select_mailbox" },
  "Navigate emails": { badge: "key" },
  "Jump to top / bottom": { id: "list_top" },
  "Half-page down / up": { id: "half_page_down" },
  "Page down / up": { id: "page_down" },
  "Jump to date (e.g. last week)": { badge: "soon" },
  "Toggle selection": { id: "mark_toggle" },
  "Select all visible": { id: "mark_all" },
  "Edit recipients (Drafts only)": { id: "edit_recipients" },
  "Attach file to draft (Drafts only)": { id: "attach_file" },
  "Approve draft (Drafts only)": { id: "approve" },
  "Unapprove, back to draft (Drafts only)": { id: "demote" },
  "Send all approved drafts (Drafts only)": { id: "send_all" },
  "Show flagged only (toggle)": { badge: "soon" },
  "Scroll headers": { badge: "key" },
  "Scroll line by line": { badge: "key" },
};

/** A section none of whose rows run from the palette. */
const BY_SECTION: Record<string, Badge> = {};

/** A section's own reading of a shared description. */
const SECTION_OVERRIDES: Record<string, Record<string, Binding>> = {
  BODY: { "Half-page down / up": { badge: "key" } },
  // The activity log dialog's own keys (ActivityLogDialog.tsx), which act
  // inside the open dialog only.
  "ACTIVITY LOG": {
    "Scroll line by line": { badge: "key" },
    "Half-page down / up": { badge: "key" },
    "Jump to top / bottom": { badge: "key" },
    "Filter entries": { badge: "key" },
    "Close overlay": { badge: "key" },
  },
  // The Calendar view's own keys (src/keymap/viewKeys.ts); its moves are
  // the list's, which reach the view's cursor.
  CALENDAR: {
    "Navigate events": { badge: "key" },
    "Jump to top / bottom": { id: "list_top" },
    "Open the invite email in $EDITOR": { id: "calendar_open_source" },
    "RSVP to invitation (Accept/Tentative/Decline)": { id: "calendar_rsvp" },
    "Show past events / upcoming only": { id: "calendar_toggle_past" },
    "Refresh events from disk": { id: "calendar_refresh" },
  },
  // The Contacts view's own keys (src/keymap/viewKeys.ts); its moves are
  // the list's, which reach the view's cursor.
  CONTACTS: {
    "Navigate contacts": { badge: "key" },
    "Jump to top / bottom": { id: "list_top" },
    "Fuzzy search": { id: "contacts_search" },
    "Compose to contact": { id: "contacts_compose" },
    "Send contact as vCard": { id: "contacts_vcard" },
    "Copy email address": { id: "contacts_copy" },
    "Refresh contact index": { id: "contacts_rebuild" },
  },
  // The results list is the message list pane while a search shows, so its
  // moves are the list's; what acts on a hit arrives with its milestone.
  "SERVER SEARCH": {
    "Navigate results": { badge: "key" },
    "Jump to top / bottom": { id: "list_top" },
    "Half-page down / up": { badge: "key" },
    "Open in the mail list": { badge: "later" },
    "Open read-only in $EDITOR": { badge: "later" },
    "Copy the Markdown rendition path": { badge: "later" },
    // `f` is the find family's prefix in the desktop client (`fm`, `ff`), so
    // the fetch is `F`, free in every mail context of the TUI.
    "Fetch a server-only hit into the store": { id: "fetch_hit", keys: ["F"] },
    // The overlay's own `b`, `o` and `O`: a hit is a list row, so the
    // MESSAGE keys act on it, a server-only hit's markup included.
    "Open HTML in browser": { id: "open_html", keys: ["tb"] },
    "Open attachment": { id: "open_attachment", keys: ["to"] },
    "Save attachment to disk": { id: "save_attachment", keys: ["ts"] },
    // The overlay's own `w`: in the desktop client a hit is a list row, so
    // `cf` forwards it (MESSAGE).
    Forward: { badge: "later" },
  },
};

export function bindingFor(section: string, action: string): Binding {
  return (
    SECTION_OVERRIDES[section]?.[action] ??
    (BY_SECTION[section] ? { badge: BY_SECTION[section] } : undefined) ??
    BY_ACTION[action] ?? { badge: "later" }
  );
}

export type PaletteEntry = {
  section: string;
  label: string;
  keys: string[];
  id: ActionId | null;
  badge: Badge | null;
};

/**
 * Every KEYMAP row, one entry per description within a section (`:` and
 * `Ctrl+p` are one entry with two keys), in KEYMAP order.
 */
export function paletteEntries(sections: KeymapSection[] = SECTIONS): PaletteEntry[] {
  const out: PaletteEntry[] = [];
  for (const s of sections) {
    const seen = new Map<string, PaletteEntry>();
    for (const b of s.bindings) {
      const prev = seen.get(b.action);
      if (prev) {
        const binding = bindingFor(s.title, b.action);
        if (!("keys" in binding && binding.keys)) prev.keys.push(b.key);
        continue;
      }
      const binding = bindingFor(s.title, b.action);
      const entry: PaletteEntry = {
        section: s.title,
        label: b.action,
        keys: "keys" in binding && binding.keys ? [...binding.keys] : [b.key],
        id: "id" in binding ? binding.id : null,
        badge: "badge" in binding ? binding.badge : null,
      };
      seen.set(b.action, entry);
      out.push(entry);
    }
  }
  return out;
}

/** The GUI-only rows the palette adds: menu items with no KEYMAP key. */
export const GUI_ENTRIES: PaletteEntry[] = [
  { section: "APP", label: "Toggle sidebar", keys: ["Cmd+b"], id: "toggle_sidebar", badge: null },
  { section: "APP", label: "Widen list", keys: [], id: "widen_list", badge: null },
  { section: "APP", label: "Narrow list", keys: [], id: "narrow_list", badge: null },
  { section: "APP", label: "Restart daemon", keys: [], id: "restart_daemon", badge: null },
  { section: "APP", label: "Back", keys: ["Alt+Left"], id: "back", badge: null },
  // The TUI moves between panes with Tab and Shift+Tab and has a go key for
  // the sidebar only (`gm`); `gr` is free in its `g` go family, as `go` is.
  { section: "APP", label: "Focus reader", keys: ["gr"], id: "focus_reader", badge: null },
  // The TUI has no settings view, and no key for one (D11): the palette, the
  // sidebar and the app menu's "Settings…", whose Cmd+, is the macOS convention.
  { section: "APP", label: "Open settings", keys: ["Cmd+,"], id: "open_settings", badge: null },
  // The account wizard (ACC-01): the TUI has none, the CLI's `mp config
  // add-account` is its model; no key of its own (D11), as Settings.
  { section: "APP", label: "Add account", keys: [], id: "add_account", badge: null },
  // The theme (Settings, src/app/theme.ts), stored in desktop.json; no key of its own, as Settings.
  { section: "APP", label: "Theme: dark", keys: [], id: "theme_dark", badge: null },
  { section: "APP", label: "Theme: light", keys: [], id: "theme_light", badge: null },
  { section: "APP", label: "Theme: system", keys: [], id: "theme_system", badge: null },
  { section: "SEARCH", label: "Search server", keys: ["ff", "Shift+Enter"], id: "search_server", badge: null },
  { section: "SEARCH", label: "Cancel the server search", keys: [], id: "cancel_search", badge: null },
  { section: "READER", label: "Show intercepted links", keys: [], id: "show_intercepted", badge: null },
  // The reader mode (src/app/readerMode.ts), stored in desktop.json: `tt`
  // toggles it, as the TUI's thread key, which the desktop does not have yet.
  { section: "READER", label: "Toggle reader text mode", keys: ["tt"], id: "toggle_reader_mode", badge: null },
  { section: "READER", label: "Reader: HTML", keys: [], id: "reader_html", badge: null },
  { section: "READER", label: "Reader: text", keys: [], id: "reader_text", badge: null },
  // The reader toolbar's Copy menu (INT-03); no key of their own (D14), `y`
  // stays the selection's selector, which is the open message's link.
  { section: "READER", label: "Copy sender address", keys: [], id: "copy_sender", badge: null },
  { section: "READER", label: "Copy link (mp://)", keys: ["y"], id: "copy_link", badge: null },
  { section: "READER", label: "Copy subject", keys: [], id: "copy_subject", badge: null },
  { section: "EMAIL LIST", label: "Mark range (from the last mark to the cursor)", keys: ["Shift+click"], id: "mark_range", badge: null },
  { section: "EMAIL LIST", label: "Clear marks", keys: ["Esc"], id: "mark_clear", badge: null },
  { section: "EMAIL LIST", label: "Discard draft (Drafts only)", keys: ["d"], id: "delete", badge: null },
  { section: "SEND", label: "Cancel the held send", keys: ["u"], id: "cancel_hold", badge: null },
  // `X` is free in every TUI context (its `cX` is a `c` continuation) and
  // names the notice's close button.
  { section: "ACTIVITY", label: "Dismiss the newest notice", keys: ["X"], id: "dismiss_notice", badge: null },
  { section: "ACTIVITY", label: "Dismiss all notices", keys: [], id: "dismiss_all_notices", badge: null },
  // The TUI has no outbox view: `go` is free in its `g` go family, and `R`
  // in every context; `d` is the view's own reading of Delete.
  // The TUI has no invitation form (the CLI's `mp send --invite`) and no key
  // for one: the palette and the Calendar view's toolbar.
  { section: "CALENDAR", label: "New invitation", keys: [], id: "new_invitation", badge: null },
  { section: "OUTBOX", label: "Open outbox", keys: ["go"], id: "open_outbox", badge: null },
  { section: "OUTBOX", label: "Retry outbox row", keys: ["R"], id: "outbox_retry", badge: null },
  { section: "OUTBOX", label: "Discard outbox row", keys: ["d"], id: "outbox_discard", badge: null },
];

/** The KEYMAP row a single printable key names in the mail sections, if any. */
export function describeKey(key: string): PaletteEntry | null {
  const mail = ["GLOBAL", "MESSAGE (list, headers, body)", "EMAIL LIST"];
  return paletteEntries().find((e) => mail.includes(e.section) && e.keys.includes(key)) ?? null;
}
