// The GUI's view of the TUI KEYMAP: src/keymap/keymap.json is generated from
// `mp dump-keys --json` (pnpm gen:keymap), and the key help and the command
// palette both render from it. What this file adds is only the binding from
// a KEYMAP row to a GUI action, or the milestone that will bring it.

import keymap from "@/keymap/keymap.json";

export type KeymapBinding = { key: string; action: string };
export type KeymapSection = { title: string; bindings: KeymapBinding[] };

export const SECTIONS: KeymapSection[] = keymap;

/** What the M1 shell can run. */
export type ActionId =
  | "focus_next"
  | "focus_prev"
  | "focus_sidebar"
  | "focus_list"
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
  | "show_intercepted";

/**
 * Why a KEYMAP row cannot run from the palette in this build:
 * a later milestone of the plan, the next M1 unit ("soon"), a row that only
 * makes sense as a key ("key", e.g. j/k or 1-9), or one the app menu owns.
 */
export type Badge = "M2" | "M3" | "M4" | "soon" | "key" | "menu" | "later";

type Binding = { id: ActionId } | { badge: Badge };

/** By the KEYMAP description, whatever the section. */
const BY_ACTION: Record<string, Binding> = {
  Quit: { badge: "menu" },
  "Jump to mailbox": { badge: "key" },
  "Cycle focus forward": { id: "focus_next" },
  "Cycle focus backward": { id: "focus_prev" },
  "Toggle this help": { id: "toggle_help" },
  "Command palette (run an action by name)": { badge: "key" },
  "Zoom / unzoom the focused pane": { id: "toggle_zoom" },
  "Toggle activity log": { badge: "M4" },
  "Send current draft (approve + send)": { badge: "M3" },
  "Search all mail (sender, subject, body)": { id: "search_server" },
  "New draft": { badge: "M3" },
  "Manage signatures": { badge: "M4" },
  "Go to mailboxes (sidebar)": { id: "focus_sidebar" },
  "Switch account": { id: "next_account" },
  "Quick sync": { badge: "M2" },
  "Full sync": { badge: "M2" },
  "Activity log overlay": { badge: "M4" },
  "Open config.toml in $EDITOR": { badge: "M4" },
  "Open log file in $EDITOR": { badge: "M4" },
  "Switch to Mail view": { id: "focus_list" },
  "Switch to Contacts view": { badge: "M4" },
  "Switch to Calendar view": { badge: "M4" },
  "Next / previous message": { id: "next_message" },
  "Open in editor (mail read-only)": { id: "open_message" },
  Reply: { badge: "M3" },
  "Reply all": { badge: "M3" },
  Forward: { badge: "M3" },
  Archive: { badge: "M2" },
  Delete: { badge: "M2" },
  "Toggle read/unread": { badge: "M2" },
  "Toggle flag/star": { badge: "M2" },
  "Move to mailbox (fuzzy picker)": { badge: "M2" },
  "Copy selector (mp://)": { id: "copy_selector" },
  "Clear selection / return to list": { id: "clear_selection" },
  "Filter the current list": { id: "focus_filter" },
  "Show conversation (thread)": { badge: "later" },
  "Open attachment": { badge: "later" },
  "Save attachment to disk": { badge: "later" },
  "Open HTML in browser": { badge: "soon" },
  "RSVP to invitation (Accept/Tentative/Decline)": { badge: "M4" },
  "Navigate mailboxes": { badge: "key" },
  "Select mailbox": { id: "select_mailbox" },
  "Navigate emails": { badge: "key" },
  "Jump to top / bottom": { id: "list_top" },
  "Half-page down / up": { id: "half_page_down" },
  "Page down / up": { id: "page_down" },
  "Jump to date (e.g. last week)": { badge: "soon" },
  "Toggle selection": { badge: "M2" },
  "Select all visible": { badge: "M2" },
  "Edit recipients (Drafts only)": { badge: "M3" },
  "Attach file to draft (Drafts only)": { badge: "M3" },
  "Approve draft (Drafts only)": { badge: "M3" },
  "Unapprove, back to draft (Drafts only)": { badge: "M3" },
  "Send all approved drafts (Drafts only)": { badge: "M3" },
  "Show flagged only (toggle)": { badge: "soon" },
  "Scroll headers": { badge: "key" },
  "Scroll line by line": { badge: "key" },
};

/** A section none of whose rows run in M1 yet. */
const BY_SECTION: Record<string, Badge> = {
  CONTACTS: "M4",
  CALENDAR: "M4",
  "ACTIVITY LOG": "M4",
};

/** A section's own reading of a shared description. */
const SECTION_OVERRIDES: Record<string, Record<string, Binding>> = {
  BODY: { "Half-page down / up": { badge: "key" } },
  // The results list is the message list pane while a search shows, so its
  // moves are the list's; what acts on a hit arrives with its milestone.
  "SERVER SEARCH": {
    "Navigate results": { badge: "key" },
    "Jump to top / bottom": { id: "list_top" },
    "Half-page down / up": { badge: "key" },
    "Open in the mail list": { badge: "later" },
    "Open read-only in $EDITOR": { badge: "later" },
    "Copy the Markdown rendition path": { badge: "later" },
    "Fetch a server-only hit into the store": { badge: "M2" },
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
        prev.keys.push(b.key);
        continue;
      }
      const binding = bindingFor(s.title, b.action);
      const entry: PaletteEntry = {
        section: s.title,
        label: b.action,
        keys: [b.key],
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
  { section: "SEARCH", label: "Search server", keys: ["ff", "Shift+Enter"], id: "search_server", badge: null },
  { section: "SEARCH", label: "Cancel the server search", keys: [], id: "cancel_search", badge: null },
  { section: "READER", label: "Show intercepted links", keys: [], id: "show_intercepted", badge: null },
];

/** The KEYMAP row a single printable key names in the mail sections, if any. */
export function describeKey(key: string): PaletteEntry | null {
  const mail = ["GLOBAL", "MESSAGE (list, headers, body)", "EMAIL LIST"];
  return paletteEntries().find((e) => mail.includes(e.section) && e.keys.includes(key)) ?? null;
}
