// A notice that leaves by itself (clients/desktop/docs/shell.md, "Actions,
// dialogs and the activity area"): a timer of `ms` that calls `onDone` once,
// paused while the pointer rests on the notice so it never vanishes under the
// cursor, and resumed with the time it had left when the pointer leaves.

import { useEffect, useRef, useState } from "react";

export type WindDownHandlers = { onPointerEnter: () => void; onPointerLeave: () => void };

/**
 * `ms` null keeps the notice until something else removes it. A change of
 * `ms` or of `key` (a new entry in the same notice) starts the full time
 * again; a new `onDone` keeps the time left.
 */
export function useWindDown(ms: number | null, onDone: () => void, key?: unknown): WindDownHandlers {
  const [hovered, setHovered] = useState(false);
  const left = useRef(0);
  const armed = useRef<{ ms: number | null; key: unknown } | null>(null);
  const fired = useRef(false);
  useEffect(() => {
    if (ms === null) return;
    if (armed.current === null || armed.current.ms !== ms || armed.current.key !== key) {
      armed.current = { ms, key };
      left.current = ms;
      fired.current = false;
    }
    if (hovered || fired.current) return;
    const start = Date.now();
    const t = setTimeout(() => {
      fired.current = true;
      onDone();
    }, left.current);
    return () => {
      clearTimeout(t);
      left.current = Math.max(0, left.current - (Date.now() - start));
    };
  }, [ms, key, hovered, onDone]);
  return { onPointerEnter: () => setHovered(true), onPointerLeave: () => setHovered(false) };
}
