// Every copy the desktop client makes: the selector (`y`), a refused link,
// and what M4 adds (a contact's address, the reader's copies, the sign-in
// code). One place, so a swap to the Tauri clipboard plugin is one file.

import type { Dispatch } from "react";
import type { Action } from "@/app/reducer";

export const CLIPBOARD_UNAVAILABLE = "The clipboard is not available";

/**
 * Copy `text` and say so in the notice line: "Copied <what>", or "The
 * clipboard refused <what>" when the webview refuses the write. Call it
 * synchronously from the key or click handler: the write needs the user
 * activation, which a copy after an `await` may have lost. The promise
 * settles once the notice is dispatched, true when the text was copied.
 */
export function copyText(text: string, what: string, dispatch: Dispatch<Action>): Promise<boolean> {
  const clipboard = typeof navigator === "undefined" ? undefined : navigator.clipboard;
  if (!clipboard?.writeText) {
    dispatch({ type: "notice", text: CLIPBOARD_UNAVAILABLE });
    return Promise.resolve(false);
  }
  let write: Promise<void>;
  try {
    write = clipboard.writeText(text);
  } catch {
    write = Promise.reject(new Error("refused"));
  }
  return write.then(
    () => {
      dispatch({ type: "notice", text: `Copied ${what}` });
      return true;
    },
    () => {
      dispatch({ type: "notice", text: `The clipboard refused ${what}` });
      return false;
    },
  );
}
