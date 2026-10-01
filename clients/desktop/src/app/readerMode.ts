// The reader mode (clients/desktop/docs/reader.md, "Text mode"): `html`, the
// message's own markup in the sandboxed frame, or `text`, the stored plain
// text drawn on the app's surface as the TUI's preview shows it. Stored as the
// `reader_mode` key of desktop.json, read once at startup and kept in the
// store; `tt`, the toolbar's HTML | Text, Settings and the palette change it.

import { useEffect, type Dispatch } from "react";
import type { Action } from "@/app/reducer";
import type { AppState } from "@/app/state";
import * as cmd from "@/lib/commands";
import { asGuiError } from "@/lib/gui-types";

export type ReaderMode = "html" | "text";

export const READER_MODES: ReaderMode[] = ["html", "text"];

/** What an unset or unreadable setting means. */
export const DEFAULT_READER_MODE: ReaderMode = "html";

export const READER_MODE_LABELS: Record<ReaderMode, string> = { html: "HTML", text: "Text" };

/** A stored value as a mode; anything else is the default. */
export function parseReaderMode(value: string | null | undefined): ReaderMode {
  return READER_MODES.includes(value as ReaderMode) ? (value as ReaderMode) : DEFAULT_READER_MODE;
}

/** The stored mode; the default when unset or when the file does not read. */
export async function loadReaderMode(): Promise<ReaderMode> {
  try {
    return parseReaderMode(await cmd.settingGet("reader_mode"));
  } catch {
    return DEFAULT_READER_MODE;
  }
}

/** Keep the stored mode in the store once `setting_get` answers; html until then. */
export function useStoredReaderMode(dispatch: Dispatch<Action>): void {
  useEffect(() => {
    let live = true;
    void loadReaderMode().then((mode) => {
      if (live) dispatch({ type: "reader_mode_set", mode });
    });
    return () => {
      live = false;
    };
  }, [dispatch]);
}

/** Show `mode` at once, then store it; a refused write says so on the notice line. */
export async function saveReaderMode(dispatch: Dispatch<Action>, mode: ReaderMode): Promise<void> {
  dispatch({ type: "reader_mode_set", mode });
  try {
    await cmd.settingSet("reader_mode", mode);
  } catch (e: unknown) {
    dispatch({ type: "notice", text: `The reader mode was not saved: ${asGuiError(e).message}`, level: "error" });
  }
}

/** `tt`: the other mode. */
export function toggleReaderMode(s: AppState, dispatch: Dispatch<Action>): Promise<void> {
  return saveReaderMode(dispatch, s.readerMode === "html" ? "text" : "html");
}

// ---------------------------------------------------------------------------
// The text body
// ---------------------------------------------------------------------------

/** What `message_text` answered for one message: its body, or null when the store holds none. */
type CachedText = { body: string | null };

const TEXT_CACHE_SIZE = 32;

/**
 * The bodies read so far, keyed by `<instance>/<readerKey>/<Message-ID>@<loadedGen>`:
 * a reload of the message reads its text again, and a row id a restarted
 * daemon gave to another message misses.
 */
const textCache = new Map<string, CachedText>();

export function cachedText(key: string): CachedText | undefined {
  return textCache.get(key);
}

export function rememberText(key: string, text: CachedText): void {
  textCache.delete(key);
  textCache.set(key, text);
  while (textCache.size > TEXT_CACHE_SIZE) {
    const oldest = textCache.keys().next().value;
    if (oldest === undefined) break;
    textCache.delete(oldest);
  }
}

/** Forget every body read; a test starts with none. */
export function forgetTexts(): void {
  textCache.clear();
}

/** A line the sender quoted: `>` after optional leading blanks, as the TUI's preview marks it. */
export function isQuoted(line: string): boolean {
  return /^\s*>/.test(line);
}
