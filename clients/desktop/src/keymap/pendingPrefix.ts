// The armed family prefix (`g`, `f`, `c`, `t`, `s`, Space), published for the
// which-key popup (components/palette/PrefixPopup.tsx). The keymap keeps its
// own ref for the hot path and writes here on every arm and disarm; only the
// popup subscribes, so a keypress never re-renders the rest of the shell.

import { useSyncExternalStore } from "react";

export type PendingPrefix = { key: string; at: number };

let current: PendingPrefix | null = null;
const listeners = new Set<() => void>();

function subscribe(listener: () => void): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

/** The prefix armed now, or null. */
export function getPendingPrefix(): PendingPrefix | null {
  return current;
}

/** Publish the armed prefix (or null once it resolves, expires or is dropped). */
export function setPendingPrefix(p: PendingPrefix | null): void {
  if (p === current) return;
  current = p;
  for (const l of listeners) l();
}

/** The armed prefix, re-rendering the caller when it changes. */
export function usePendingPrefix(): PendingPrefix | null {
  return useSyncExternalStore(subscribe, getPendingPrefix, getPendingPrefix);
}
