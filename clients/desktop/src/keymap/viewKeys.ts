// The keys of the full-pane views, which the keymap consults before any
// family prefix arms, the TUI's view rule (clients/tui/src/app/keys.rs,
// `is_view_agnostic` and `leader_is_view_agnostic` in keymap.rs): a view may
// claim `c`, `t`, `a` or `r` for itself without arming the mail families, and
// only the families with a view-agnostic continuation (`s`, Space) still arm.

import type { View } from "@/app/state";
import type { ActionId } from "@/keymap/catalog";

/** How one full-pane view reads the keyboard. */
export type ViewKeyTable = {
  /** Single keys the view binds, checked before any prefix arms. */
  keys: Readonly<Record<string, ActionId>>;
  /** The prefixes that still arm in the view: `s` and Space always, `g` only for a view's own `g` combos. */
  prefixes: ReadonlySet<string>;
  /** Continuations of an armed prefix that the view binds, such as `gg`. */
  combos: Readonly<Record<string, ActionId>>;
};

/**
 * The prefix continuations that run in every view, Mail included: the
 * syncs and the view switch. U6's `sl`, `sc` and `sf` belong here too.
 */
export const VIEW_AGNOSTIC_COMBOS: Readonly<Record<string, ActionId>> = {
  ss: "quick_sync",
  sS: "full_sync",
  "Space m": "view_mail",
  "Space c": "view_contacts",
  "Space a": "view_calendar",
};

/**
 * The keys a view leaves to the shared routing: pane cycling, the palette,
 * key help, the notice key and the moves, which reach the view's cursor
 * through `move_selection`. Every other printable key does nothing unless
 * the view's table binds it.
 */
export const VIEW_SHARED_KEYS: ReadonlySet<string> = new Set([
  "Tab",
  ":",
  "?",
  "X",
  "j",
  "k",
  "G",
  "ArrowDown",
  "ArrowUp",
  "PageDown",
  "PageUp",
  "Home",
  "End",
]);

const ARM_ALWAYS = ["s", " "];

/** A list view's table: `g` arms only for its own `gg`, the TUI's CONTACTS and CALENDAR `gg / G`. */
function listView(keys: Record<string, ActionId> = {}): ViewKeyTable {
  return { keys, prefixes: new Set([...ARM_ALWAYS, "g"]), combos: { gg: "list_top" } };
}

/** Each full-pane view's table. Settings owns no letter keys. */
export const VIEW_KEYS: Readonly<Record<Exclude<View, "mail">, ViewKeyTable>> = {
  // The TUI's CONTACTS (keymap.rs): `/` focuses the search field, Enter and
  // `n` compose to the cursor contact, `v` sends it as a vCard, `c` copies
  // its address and never arms the `c` family (so `cn` and `cs` are not
  // reachable here, as in the TUI), `r` rebuilds the index; `j`/`k`, `gg`,
  // `G` move its cursor.
  contacts: listView({
    "/": "contacts_search",
    Enter: "contacts_compose",
    n: "contacts_compose",
    v: "contacts_vcard",
    c: "contacts_copy",
    r: "contacts_rebuild",
  }),
  // The TUI's CALENDAR (keymap.rs): Enter and `e` open the entry's
  // invite.ics, `t` shows or hides past events and never arms the `t`
  // family, `r` reads the agenda again, `V` opens the RSVP choice for the
  // cursor row; `j`/`k`, `gg`, `G` move its cursor.
  calendar: listView({
    Enter: "calendar_open_source",
    e: "calendar_open_source",
    t: "calendar_toggle_past",
    r: "calendar_refresh",
    V: "calendar_rsvp",
  }),
  settings: { keys: {}, prefixes: new Set(ARM_ALWAYS), combos: {} },
};

/** The table of the view shown, or null in Mail, whose keys are the keymap's own. */
export function viewKeyTable(view: View): ViewKeyTable | null {
  return view === "mail" ? null : VIEW_KEYS[view];
}
