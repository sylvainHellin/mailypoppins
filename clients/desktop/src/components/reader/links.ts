// The two things a user may do with a link the reader refused: open it in
// the default browser, which happens only here and only on an explicit click,
// or copy it.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";
import * as cmd from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";

/** What `open_external` accepts; anything else is refused by the Rust layer too. */
export function canOpenExternally(url: string): boolean {
  return /^(https?:|mailto:)/i.test(url);
}

export function openInBrowser(url: string, dispatch: Dispatch<Action>): void {
  cmd
    .openExternal(url)
    .then(() => dispatch({ type: "notice", text: "Opened in the browser" }))
    .catch((e: unknown) => dispatch({ type: "notice", text: `Could not open the link: ${asGuiError(e).message}` }));
}

export function copyLink(url: string, dispatch: Dispatch<Action>): void {
  const clipboard = navigator.clipboard;
  if (!clipboard) {
    dispatch({ type: "notice", text: "The clipboard is not available" });
    return;
  }
  clipboard
    .writeText(url)
    .then(() => dispatch({ type: "notice", text: "Copied the link" }))
    .catch(() => dispatch({ type: "notice", text: "The clipboard refused the link" }));
}
