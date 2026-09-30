// Runs a GUI action, whichever path asked: a key, the palette, a native menu
// item, or a button. One table, so no path can do something another cannot.

import { useCallback, useRef, type Dispatch } from "react";
import type { Action } from "@/app/reducer";
import { LIST_WIDTH_STEP, type AppState } from "@/app/state";
import type { ActionId } from "@/keymap/catalog";
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

export function runAction(id: ActionId, s: AppState, dispatch: Dispatch<Action>): void {
  switch (id) {
    case "focus_next":
      return dispatch({ type: "cycle_focus", dir: 1 });
    case "focus_prev":
      return dispatch({ type: "cycle_focus", dir: -1 });
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
      if (s.selection.message || s.selection.draft) dispatch({ type: "focus", pane: "reader" });
      return;
    case "select_mailbox":
      return dispatch({ type: "sidebar_enter" });
    case "copy_selector": {
      const selector = s.selection.message?.selector;
      if (!selector) return;
      void navigator.clipboard
        ?.writeText(selector)
        .then(() => dispatch({ type: "notice", text: `Copied ${selector}` }))
        .catch(() => dispatch({ type: "notice", text: "The clipboard refused the selector" }));
      return;
    }
    case "clear_selection":
      return dispatch({ type: "clear_selection" });
    case "focus_filter":
      dispatch({ type: "focus", pane: "list" });
      queueMicrotask(() => document.getElementById(FILTER_INPUT_ID)?.focus());
      return;
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
    // The splitter's keyboard road: the reducer clamps to the width range.
    case "widen_list":
      return dispatch({ type: "set_list_width", px: s.prefs.listWidth + LIST_WIDTH_STEP });
    case "narrow_list":
      return dispatch({ type: "set_list_width", px: s.prefs.listWidth - LIST_WIDTH_STEP });
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
  }
}

/** A stable runner bound to the latest state. */
export function useRunAction(state: AppState, dispatch: Dispatch<Action>): (id: ActionId) => void {
  const ref = useRef(state);
  ref.current = state;
  return useCallback((id: ActionId) => runAction(id, ref.current, dispatch), [dispatch]);
}

export { HALF_PAGE, PAGE };
