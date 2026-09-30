// The one ordered event channel: `subscribe_events` with a Channel<GuiEvent>.
// A second subscription replaces the first on the Rust side, so the app
// subscribes once, at the root.

import { Channel, listen, type UnlistenFn } from "@/lib/tauri";
import { subscribeEvents } from "@/lib/commands";
import type { GuiEvent } from "@/lib/gui-types";

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
