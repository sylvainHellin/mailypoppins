// The embedded terminal's font (clients/desktop/docs/shell.md, "The terminal
// pane"): a family and a size, each optional, stored as the
// `terminal_font_family` and `terminal_font_size` keys of desktop.json, read
// once at startup into `state.terminalFont` and changed in Settings. Unset,
// the terminal draws in the app's `font-mono` at `text-sm`; a family goes
// before that stack and a size replaces the host's (terminalTheme.ts's
// `terminalFont`). Every open pane takes a change at once.
//
// The rules (src/lib/terminal-font.ts) match settings.rs's `family_is_safe`
// and `size_is_valid`, which refuse a write that breaks them; a stored value
// that breaks them anyway reads as unset.

import { useEffect, type Dispatch } from "react";
import type { Action } from "@/app/reducer";
import * as cmd from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";
import {
  DEFAULT_TERMINAL_FONT,
  familyRefusal,
  parseFontFamily,
  parseFontSize,
  sizeRefusal,
  type TerminalFontSetting,
} from "@/lib/terminal-font";

/** The stored setting; unset where the file does not read. */
export async function loadTerminalFont(): Promise<TerminalFontSetting> {
  try {
    const [family, size] = await Promise.all([cmd.settingGet("terminal_font_family"), cmd.settingGet("terminal_font_size")]);
    return { family: parseFontFamily(family), size: parseFontSize(size) };
  } catch {
    return DEFAULT_TERMINAL_FONT;
  }
}

/** Keep the stored setting in the store once `setting_get` answers; unset until then. */
export function useStoredTerminalFont(dispatch: Dispatch<Action>): void {
  useEffect(() => {
    let live = true;
    void loadTerminalFont().then((font) => {
      if (live) dispatch({ type: "terminal_font_set", font });
    });
    return () => {
      live = false;
    };
  }, [dispatch]);
}

/**
 * Store the family and the size typed in Settings, a blank field clearing
 * its key, and put what desktop.json holds afterwards in the store, where
 * every open pane takes it. Neither key is written while either value is
 * refused. Answers the refusal's sentence for the form to show, or null.
 */
export async function saveTerminalFont(dispatch: Dispatch<Action>, family: string, size: string): Promise<string | null> {
  const refused = familyRefusal(family) ?? sizeRefusal(size);
  if (refused) return refused;
  try {
    const storedFamily = await cmd.settingSet("terminal_font_family", family.trim() || null);
    const storedSize = await cmd.settingSet("terminal_font_size", size.trim() || null);
    const font = { family: parseFontFamily(storedFamily), size: parseFontSize(storedSize) };
    dispatch({ type: "terminal_font_set", font });
    dispatch({ type: "notice", text: terminalFontNotice(font) });
    return null;
  } catch (e: unknown) {
    return asGuiError(e).message;
  }
}

/** What the notice line says once the setting is stored. */
export function terminalFontNotice(font: TerminalFontSetting): string {
  const family = font.family ?? "the app's mono font";
  const size = font.size === null ? "its usual size" : `${font.size} px`;
  return `The terminal draws in ${family} at ${size}`;
}
