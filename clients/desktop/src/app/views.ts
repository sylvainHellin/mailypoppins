// The views beside Mail: Contacts, Calendar and Settings, each a full pane in
// place of the list and the reader (clients/desktop/docs/shell.md, "Views").
// Pure; the reducer's `switch_view` and the keymap's view block use it.

import { CLOSE_OUTBOX_FIRST, hiddenByOutbox, SELECTION_ACTIONS } from "@/app/outbox";
import { PANES, type AppState, type Pane, type View } from "@/app/state";
import type { ActionId } from "@/keymap/catalog";

export const VIEW_TITLES: Record<View, string> = {
  mail: "Mail",
  contacts: "Contacts",
  calendar: "Calendar",
  settings: "Settings",
};

/** The action that shows each view, what its sidebar entry, its keys and its palette row run. */
export const VIEW_ACTIONS: Record<View, ActionId> = {
  mail: "view_mail",
  contacts: "view_contacts",
  calendar: "view_calendar",
  settings: "open_settings",
};

export const BACK_TO_MAIL_FIRST = "Go back to Mail first (Escape): this acts on the mailbox selection";

/**
 * Whether a full-pane view hides what `id` acts on: outside Mail the mailbox
 * selection and its marks are out of sight, so the actions that read them
 * ({@link SELECTION_ACTIONS}, the outbox view's set) do nothing.
 */
export function hiddenByView(s: AppState, id: ActionId): boolean {
  return s.view !== "mail" && SELECTION_ACTIONS.has(id);
}

/**
 * Why `id` cannot run over what the window shows, or null when it can: a
 * full-pane view first, then the outbox view inside Mail. Keys drop such an
 * action silently, and the palette and the menu say this.
 */
export function hiddenNotice(s: AppState, id: ActionId): string | null {
  if (hiddenByView(s, id)) return BACK_TO_MAIL_FIRST;
  if (hiddenByOutbox(s, id)) return CLOSE_OUTBOX_FIRST;
  return null;
}

/** The panes Tab cycles: a full-pane view has no reader. */
export function viewPanes(s: AppState): readonly Pane[] {
  return s.view === "mail" ? PANES : ["sidebar", "list"];
}
