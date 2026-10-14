// The preferences the tests start from (ticket 0142). The app lists every
// account by conversation unless the prefs name it flat; the tests written
// against the flat list start from the fixture accounts listed flat, and a
// test of the conversation list opts in with `initialState()`.

import accountsFx from "../../fixtures/accounts.json";
import { DEFAULT_PREFS, initialState, type AppState, type Prefs } from "@/app/state";

/** Every fixture account listed one row per message. */
export const FLAT_PREFS: Prefs = { ...DEFAULT_PREFS, flatListAccounts: accountsFx.accounts.map((a) => a.name) };

/** `initialState()` with every fixture account listed flat. */
export function flatState(): AppState {
  return initialState(FLAT_PREFS);
}
