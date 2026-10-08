// The one ordered event channel: `subscribe_events` with a Channel<GuiEvent>.
// A second subscription replaces the first on the Rust side, so the app
// subscribes once, at the root. The native menu and the updater speak
// through app events (`listen`) instead.

import { Channel, listen, type UnlistenFn } from "@/lib/tauri";
import { subscribeEvents } from "@/lib/commands";
import type { GuiEvent, UpdateAvailable } from "@/lib/gui-types";

export function subscribe(onEvent: (event: GuiEvent) => void): Promise<Channel<GuiEvent>> {
  const channel = new Channel<GuiEvent>();
  channel.onmessage = onEvent;
  return subscribeEvents(channel).then(() => channel);
}

/** The native menu's item ids, emitted by src-tauri/src/menu.rs as `menu`. */
export const MENU_EVENT = "menu";

export function onMenu(handler: (id: string) => void): Promise<UnlistenFn> {
  return listen<string>(MENU_EVENT, (e) => handler(e.payload));
}

/** The silent startup check found a version the user did not skip (src-tauri/src/updates.rs). */
export const UPDATE_AVAILABLE_EVENT = "update:available";

/** The App menu's "Check for Updates…", which runs the manual check. */
export const UPDATE_CHECK_REQUESTED_EVENT = "update:check_requested";

export function onUpdateAvailable(handler: (update: UpdateAvailable) => void): Promise<UnlistenFn> {
  return listen<UpdateAvailable>(UPDATE_AVAILABLE_EVENT, (e) => handler(e.payload));
}

export function onUpdateCheckRequested(handler: () => void): Promise<UnlistenFn> {
  return listen<null>(UPDATE_CHECK_REQUESTED_EVENT, () => handler());
}
