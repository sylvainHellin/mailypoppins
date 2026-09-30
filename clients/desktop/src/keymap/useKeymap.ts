// Keyboard routing the way the TUI routes it: global keys first, then the
// focused pane's. Keys are ignored while a text field has focus, except
// Escape, which leaves the field; an open dialog owns every key.

import { useEffect, useRef, type Dispatch } from "react";
import type { Action } from "@/app/reducer";
import { liveHolds, screenFor, type AppState } from "@/app/state";
import { FILTER_INPUT_ID, HALF_PAGE, PAGE, READER_SCROLL_ID, runAction } from "@/app/actions";
import { describeKey, type ActionId, type Badge } from "@/keymap/catalog";

const PREFIX_TIMEOUT_MS = 1200;
const PREFIXES = new Set(["g", "f", "c", "t", "s"]);
const LINE_PX = 48;

const MESSAGE_KEYS: Record<string, ActionId> = {
  a: "archive",
  d: "delete",
  u: "toggle_read",
  "*": "toggle_flag",
  M: "move",
};

const BADGE_TEXT: Record<Badge, string> = {
  M3: "arrives with compose (M3)",
  M4: "arrives in M4",
  soon: "arrives in the next M1 unit",
  key: "is a key binding",
  menu: "is in the app menu",
  later: "is not in the desktop client yet",
};

export function isEditable(el: EventTarget | null): boolean {
  if (!(el instanceof HTMLElement)) return false;
  if (el.isContentEditable) return true;
  if (el instanceof HTMLTextAreaElement || el instanceof HTMLSelectElement) return true;
  if (el instanceof HTMLInputElement) {
    return !["button", "checkbox", "radio", "submit", "reset", "range", "color"].includes(el.type);
  }
  return false;
}

function scrollReader(by: number | "top" | "bottom"): void {
  const el = document.getElementById(READER_SCROLL_ID);
  if (!el) return;
  if (by === "top") el.scrollTop = 0;
  else if (by === "bottom") el.scrollTop = el.scrollHeight;
  else el.scrollTop += by;
}

/** Tab cycles panes only from a pane (or nothing); elsewhere it is the browser's. */
function tabIsOurs(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return true;
  if (target === document.body) return true;
  return target.closest("[data-pane]") !== null;
}

export function useKeymap(state: AppState, dispatch: Dispatch<Action>): void {
  const ref = useRef(state);
  ref.current = state;
  const prefix = useRef<{ key: string; at: number } | null>(null);

  useEffect(() => {
    const run = (id: ActionId) => runAction(id, ref.current, dispatch);
    const notice = (combo: string) => {
      const entry = describeKey(combo);
      if (entry && entry.badge) {
        dispatch({ type: "notice", text: `${entry.label} ${BADGE_TEXT[entry.badge]}` });
      }
    };

    const onKey = (e: KeyboardEvent) => {
      const s = ref.current;
      if (e.defaultPrevented || e.isComposing) return;
      if (["Shift", "Control", "Alt", "Meta"].includes(e.key)) return;
      if (s.overlay !== null) return;
      if (screenFor(s) !== "shell") return;

      if (isEditable(e.target)) {
        if (e.key === "Escape") {
          e.preventDefault();
          (e.target as HTMLElement).blur();
          if ((e.target as HTMLElement).id === FILTER_INPUT_ID) {
            // Out of the field, and out of a search: the mailbox list returns.
            dispatch(s.search ? { type: "exit_search" } : { type: "focus", pane: "list" });
          }
        }
        return;
      }

      const handled = () => e.preventDefault();

      // History: Cmd+[ and Alt+Left go back, as a browser does.
      if ((e.metaKey && e.key === "[") || (e.altKey && e.key === "ArrowLeft")) {
        handled();
        return run("back");
      }
      if (e.metaKey || e.altKey) return;

      if (e.ctrlKey) {
        const k = e.key.toLowerCase();
        if (k === "p") return handled(), run("open_palette");
        if (k === "d") return handled(), pageBy(HALF_PAGE);
        if (k === "u") return handled(), pageBy(-HALF_PAGE);
        // The TUI's Ctrl+a is a List key.
        if (k === "a") return handled(), s.focus === "list" && run("mark_all");
        return;
      }

      // A pending family prefix (`g`, `f`, `c`, `t`, `s`, Space).
      const pending = prefix.current;
      prefix.current = null;
      if (pending && Date.now() - pending.at < PREFIX_TIMEOUT_MS) {
        handled();
        const combo = pending.key === " " ? `Space ${e.key}` : `${pending.key}${e.key}`;
        switch (combo) {
          case "gg":
            return run("list_top");
          case "gm":
            return run("focus_sidebar");
          case "ga":
            return run("next_account");
          case "gj":
            return run("next_message");
          case "gk":
            return run("prev_message");
          case "fm":
            return run("focus_filter");
          case "ff":
            return run("search_server");
          case "Space m":
            return run("focus_list");
          case "ss":
            return run("quick_sync");
          case "sS":
            return run("full_sync");
          default:
            return notice(combo);
        }
      }
      if (PREFIXES.has(e.key) || e.key === " ") {
        handled();
        prefix.current = { key: e.key, at: Date.now() };
        return;
      }

      function pageBy(n: number) {
        if (ref.current.focus === "reader") scrollReader(n * LINE_PX);
        else dispatch({ type: "move_selection", to: n, relative: true });
      }

      function move(delta: 1 | -1) {
        const f = ref.current.focus;
        if (f === "sidebar") dispatch({ type: "move_sidebar_cursor", delta });
        else if (f === "list") dispatch({ type: "move_selection", to: delta, relative: true });
        else scrollReader(delta * LINE_PX);
      }

      switch (e.key) {
        case "Tab":
          if (!tabIsOurs(e.target)) return;
          handled();
          return run(e.shiftKey ? "focus_prev" : "focus_next");
        case ":":
          return handled(), run("open_palette");
        case "?":
          return handled(), run("toggle_help");
        case "z":
          return handled(), run("toggle_zoom");
        case "/":
          return handled(), run("focus_filter");
        case "J":
          return handled(), run("next_message");
        case "K":
          return handled(), run("prev_message");
        case "G":
          handled();
          if (s.focus === "reader") return scrollReader("bottom");
          return run("list_bottom");
        case "y":
          return handled(), run("copy_selector");
        // Desktop only: the activity area sits outside every pane, so Tab never reaches it.
        case "X":
          return handled(), run("dismiss_notice");
        case "Escape":
          handled();
          if (s.marked.keys.size > 0) return run("mark_clear");
          if (s.search && s.focus !== "reader") return dispatch({ type: "exit_search" });
          if (s.layout === "narrow") return dispatch({ type: "up" });
          return run("clear_selection");
        case "Enter":
          if (s.focus === "sidebar") return handled(), run("select_mailbox");
          if (s.focus === "list") return handled(), run("open_message");
          return;
        case "j":
        case "ArrowDown":
          return handled(), move(1);
        case "k":
        case "ArrowUp":
          return handled(), move(-1);
        case "PageDown":
          return handled(), pageBy(PAGE);
        case "PageUp":
          return handled(), pageBy(-PAGE);
        case "Home":
          handled();
          if (s.focus === "reader") return scrollReader("top");
          return run("list_top");
        case "End":
          handled();
          if (s.focus === "reader") return scrollReader("bottom");
          return run("list_bottom");
        default:
          break;
      }
      // While a send is held, `u` cancels it before it is toggle read (the TUI's rule).
      if (e.key === "u" && liveHolds(s).length > 0) return handled(), run("cancel_hold");
      // The MESSAGE keys act from the list and the reader, as the TUI's do
      // from its list, headers and body; `v` is a List key.
      const message = MESSAGE_KEYS[e.key];
      if (message) {
        handled();
        if (s.focus !== "sidebar") run(message);
        return;
      }
      if (e.key === "v") {
        handled();
        if (s.focus === "list") run("mark_toggle");
        return;
      }
      if (/^[1-9]$/.test(e.key)) {
        handled();
        return dispatch({ type: "jump_mailbox", index: Number(e.key) - 1 });
      }
      if (e.key.length === 1) notice(e.key);
    };

    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [dispatch]);
}
